//! Real TextEdit matrix. Ignored is NOT qualification. See test-artifacts/NATIVE-CONTEXT-REPORT.md.
#![cfg(target_os = "macos")]

use anyhow::{bail, ensure, Context, Result};
use app_lib::domain::ports::{ContextValidation as V, ContinuationContextGuard};
use app_lib::infrastructure::{
    auto_paste::{get_active_app_target, AutoPasteTarget},
    continuation_context::{ContinuationContextManager, GuardedPasteOutcome as P},
};
use cocoa::{
    base::{id, nil},
    foundation::NSString,
};
use objc::{class, msg_send, sel, sel_impl};
use serde_json::json;
use std::{
    ffi::{c_void, CStr},
    fs::{self, File, OpenOptions},
    future::Future,
    io::Write,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

// Objective-C autoreleased values stay alive for the entire sequential matrix.
struct Pool(id);
impl Drop for Pool {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.0, drain];
        }
    }
}
type Ref = *const c_void;
#[repr(C)]
struct Range {
    location: isize,
    length: isize,
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementSetMessagingTimeout(element: Ref, seconds: f32) -> i32;
    fn AXUIElementCopyAttributeValue(element: Ref, key: Ref, value: *mut Ref) -> i32;
    fn AXUIElementSetAttributeValue(element: Ref, key: Ref, value: Ref) -> i32;
    fn AXValueCreate(kind: u32, value: *const c_void) -> Ref;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: Ref);
    fn CFStringCreateWithBytes(a: Ref, bytes: *const u8, len: isize, enc: u32, ext: bool) -> Ref;
    fn CFStringGetLength(value: Ref) -> isize;
    fn CFStringGetCharacters(value: Ref, range: Range, buffer: *mut u16);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFStringGetTypeID() -> usize;
}
struct Owned(Ref);
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}
impl Owned {
    fn string(s: &str) -> Result<Self> {
        Self::new(unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                s.as_ptr(),
                s.len() as isize,
                0x08000100,
                false,
            )
        })
    }
    fn new(p: Ref) -> Result<Self> {
        ensure!(!p.is_null(), "native allocation unavailable");
        Ok(Self(p))
    }
    fn attr(&self, key: &str) -> Result<Self> {
        let key = Self::string(key)?;
        let mut value = std::ptr::null();
        unsafe {
            ensure!(
                AXUIElementSetMessagingTimeout(self.0, 0.2) == 0,
                "AX timeout unavailable"
            );
            ensure!(
                AXUIElementCopyAttributeValue(self.0, key.0, &mut value) == 0,
                "AX attribute unavailable"
            );
        }
        Self::new(value)
    }
    fn text(&self) -> Result<String> {
        unsafe {
            ensure!(
                CFGetTypeID(self.0) == CFStringGetTypeID(),
                "AX string required"
            );
            let len = CFStringGetLength(self.0);
            ensure!((0..=65536).contains(&len), "AX text exceeds bound");
            let mut v = vec![0; len as usize];
            CFStringGetCharacters(
                self.0,
                Range {
                    location: 0,
                    length: len,
                },
                v.as_mut_ptr(),
            );
            Ok(String::from_utf16(&v)?)
        }
    }
}
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
// Spool output to owned files, avoiding pipe-buffer deadlocks. Never include stderr/private text in errors.
fn process(root: &Path, program: &str, args: &[&str]) -> Result<String> {
    let out = root.join("process.out");
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(File::create(&out)?)
        .stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > Duration::from_secs(6) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&out);
            bail!("bounded process timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        let _ = fs::remove_file(&out);
        bail!("native process failed (AX/TCC support is required)");
    }
    ensure!(
        fs::metadata(&out)?.len() <= 65536,
        "process output exceeds bound"
    );
    let result = fs::read_to_string(&out)?;
    fs::remove_file(out)?;
    Ok(result.strip_suffix('\n').unwrap_or(&result).to_owned())
}
fn script(root: &Path, body: &str) -> Result<String> {
    process(
        root,
        "/usr/bin/osascript",
        &[
            "-e",
            &format!("with timeout of 4 seconds\n{body}\nend timeout"),
        ],
    )
}
fn doc(path: &Path) -> String {
    format!(
        "first document whose path is {}",
        quoted(path.to_str().unwrap())
    )
}
fn read(root: &Path, path: &Path) -> Result<String> {
    script(root, &format!("tell application \"TextEdit\"\nset d to {}\nif (path of d) is not {} then error \"path mismatch\"\nreturn text of d\nend tell", doc(path), quoted(path.to_str().unwrap())))
}
fn focus(root: &Path, path: &Path) -> Result<AutoPasteTarget> {
    script(
        root,
        &format!(
            "tell application \"TextEdit\"\nopen POSIX file {}\nactivate\nend tell",
            quoted(path.to_str().unwrap())
        ),
    )?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(get_active_app_target());
    });
    let target = rx
        .recv_timeout(Duration::from_secs(2))?
        .context("no active target")?;
    ensure!(
        target.bundle_id == "com.apple.TextEdit" && target.pid > 0,
        "TextEdit target required"
    );
    editor(&target, path)?;
    Ok(target)
}
fn editor(target: &AutoPasteTarget, path: &Path) -> Result<Owned> {
    let app = Owned::new(unsafe { AXUIElementCreateApplication(target.pid) })?;
    let e = app.attr("AXFocusedUIElement")?;
    let identity = e.attr("AXWindow")?.attr("AXDocument")?.text()?;
    // Canonical temp paths contain no percent escapes; accept only the exact file URL/path.
    ensure!(
        identity == format!("file://{}", path.display()) || identity == path.to_str().unwrap(),
        "focused AX document is not owned path"
    );
    Ok(e)
}
fn range(target: &AutoPasteTarget, path: &Path, location: isize, length: isize) -> Result<()> {
    let e = editor(target, path)?;
    let key = Owned::string("AXSelectedTextRange")?;
    let r = Range { location, length };
    let value = Owned::new(unsafe { AXValueCreate(4, &r as *const _ as Ref) })?;
    unsafe {
        ensure!(
            AXUIElementSetAttributeValue(e.0, key.0, value.0) == 0,
            "AX range mutation required"
        );
    }
    Ok(())
}

#[cfg(all(debug_assertions, feature = "native-window-e2e"))]
fn bind_synthetic_reader(
    path: &Path,
) -> Result<app_lib::infrastructure::auto_paste::SyntheticTextEditReader> {
    use app_lib::infrastructure::auto_paste::SyntheticTextEditReader;

    let deadline = Instant::now() + Duration::from_secs(2);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut owner = None;
    let reader = SyntheticTextEditReader::bind(path, &mut owner, 1, cancel, deadline)?;
    SyntheticTextEditReader::finish_arming();
    Ok(reader)
}

#[cfg(all(debug_assertions, feature = "native-window-e2e"))]
fn read_synthetic_reader(
    reader: &app_lib::infrastructure::auto_paste::SyntheticTextEditReader,
) -> Result<Option<String>> {
    reader.read(None, Instant::now() + Duration::from_millis(200))
}
// All representations stay in process memory; no text conversion of representation data.
// Bounds are deliberately practical, not a general pasteboard archival format.
type Image = Vec<Vec<(String, Vec<u8>)>>;
struct Clipboard {
    board: id,
    original: Image,
    revision: isize,
    dirty: bool,
    uncertain: bool,
}
impl Clipboard {
    fn snapshot() -> Result<Self> {
        unsafe {
            let board: id = msg_send![class!(NSPasteboard), generalPasteboard];
            ensure!(!board.is_null(), "clipboard unavailable");
            let mut c = Self {
                board,
                original: Vec::new(),
                revision: msg_send![board, changeCount],
                dirty: false,
                uncertain: false,
            };
            c.original = c.image()?;
            ensure!(c.count() == c.revision, "clipboard changed during snapshot");
            Ok(c)
        }
    }
    fn count(&self) -> isize {
        unsafe { msg_send![self.board, changeCount] }
    }
    fn image(&self) -> Result<Image> {
        unsafe {
            let items: id = msg_send![self.board, pasteboardItems];
            let n: usize = msg_send![items, count];
            ensure!(n <= 32, "clipboard exceeds 32 items");
            let mut image = Vec::new();
            let mut total = 0usize;
            for i in 0..n {
                let item: id = msg_send![items, objectAtIndex: i];
                let types: id = msg_send![item, types];
                let m: usize = msg_send![types, count];
                ensure!((1..=64).contains(&m), "clipboard item types outside bound");
                let mut representations = Vec::new();
                for j in 0..m {
                    let kind: id = msg_send![types, objectAtIndex: j];
                    let len: usize = msg_send![kind, length];
                    ensure!(len <= 1024, "clipboard type name exceeds bound");
                    let ptr: *const std::ffi::c_char = msg_send![kind, UTF8String];
                    ensure!(!ptr.is_null(), "clipboard type unresolved");
                    let name = CStr::from_ptr(ptr).to_str()?.to_owned();
                    let lower = name.to_ascii_lowercase();
                    ensure!(
                        !lower.contains("promise") && !lower.contains("promised"),
                        "promised clipboard representation unsupported"
                    );
                    // This may synchronously resolve a lazy provider. Nil is never flattened away.
                    let data: id = msg_send![item, dataForType: kind];
                    ensure!(!data.is_null(), "clipboard representation unresolved");
                    let len: usize = msg_send![data, length];
                    ensure!(
                        len <= 4 * 1024 * 1024,
                        "clipboard representation exceeds 4 MiB"
                    );
                    total = total.checked_add(len).context("clipboard size overflow")?;
                    ensure!(total <= 16 * 1024 * 1024, "clipboard exceeds 16 MiB");
                    let bytes: *const u8 = msg_send![data, bytes];
                    ensure!(len == 0 || !bytes.is_null(), "clipboard data unavailable");
                    let bytes = if len == 0 {
                        Vec::new()
                    } else {
                        std::slice::from_raw_parts(bytes, len).to_vec()
                    };
                    representations.push((name, bytes));
                }
                image.push(representations);
            }
            Ok(image)
        }
    }
    fn check(&self) -> Result<()> {
        ensure!(
            !self.uncertain && self.count() == self.revision,
            "clipboard ownership uncertain/concurrent change; refusing overwrite"
        );
        Ok(())
    }
    fn publish(&mut self, image: &Image) -> Result<()> {
        unsafe {
            // Prepare complete, eager items BEFORE touching the general pasteboard.
            let array: id = msg_send![class!(NSMutableArray), array];
            for representations in image {
                let item: id = msg_send![class!(NSPasteboardItem), new];
                let item: id = msg_send![item, autorelease];
                ensure!(!item.is_null(), "clipboard item allocation failed");
                for (name, bytes) in representations {
                    let kind = NSString::alloc(nil).init_str(name);
                    let data: id = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr()
                        length: bytes.len()];
                    let ok: bool = msg_send![item, setData: data forType: kind];
                    let _: () = msg_send![kind, release];
                    ensure!(ok, "clipboard item preparation failed");
                }
                let _: () = msg_send![array, addObject: item];
            }
            self.check()?;
            // NSPasteboard has no CAS: an external writer in this gap cannot be excluded.
            self.revision = msg_send![self.board, clearContents];
            self.dirty = true;
            self.check()?;
            let ok: bool = image.is_empty() || msg_send![self.board, writeObjects: array];
            ensure!(
                ok,
                "clipboard publication failed; original retained for cleanup"
            );
            self.check()?;
        }
        Ok(())
    }
    fn write(&mut self, text: &str) -> Result<()> {
        self.publish(&vec![vec![(
            "public.utf8-plain-text".into(),
            text.as_bytes().to_vec(),
        )]])
    }
    fn refused(&mut self, before: isize, outcome: &P) {
        // Only a returned pre-effect refusal with our unchanged KNOWN generation
        // proves no clipboard transaction occurred. Text equality proves nothing.
        if matches!(outcome, P::Unavailable | P::ContextMismatch)
            && before == self.revision
            && self.count() == before
        {
            self.uncertain = false;
        }
    }
    fn confirmed(&mut self, before: isize, duplicate: bool, marker: &str) -> Result<()> {
        // Production TextEdit delivery clears for insertion then clears for restoration.
        // Duplicate delivery is cached and performs no clipboard transaction.
        // Never adopt an arbitrary revision just because its text matches our marker.
        let expected = before
            .checked_add(if duplicate { 0 } else { 2 })
            .context("clipboard revision overflow")?;
        let image = self.image()?;
        ensure!(
            self.count() == expected
                && image
                    == vec![vec![(
                        "public.utf8-plain-text".into(),
                        marker.as_bytes().to_vec()
                    )]],
            "unexpected production clipboard effects; restoration refused"
        );
        self.revision = expected;
        self.uncertain = false;
        Ok(())
    }
    fn restore(&mut self) -> Result<()> {
        self.check()?;
        if self.dirty {
            self.publish(&self.original.clone())?;
            ensure!(
                self.image()? == self.original,
                "full clipboard restoration verification failed"
            );
            self.check()?;
            self.dirty = false;
        }
        Ok(())
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        if self.restore().is_err() {
            eprintln!("clipboard cleanup refused/failed; original memory retained until fixture drop; no overwrite on uncertain ownership");
        }
    }
}
async fn bounded<T>(future: impl Future<Output = T>) -> Result<T> {
    Ok(tokio::time::timeout(Duration::from_secs(4), future).await?)
}
struct Evidence {
    root: PathBuf,
    file: File,
}
impl Evidence {
    fn event(&mut self, case: &str, stage: &str, actual: String, expected: String) -> Result<()> {
        writeln!(
            self.file,
            "{}",
            json!({"case":case,"stage":stage,"actual":actual,"expected":expected,"pass":actual==expected})
        )?;
        self.file.flush()?;
        ensure!(
            actual == expected,
            "required case {case}, stage {stage} failed"
        );
        Ok(())
    }
    fn text(&mut self, case: &str, path: &Path, expected: &str) -> Result<()> {
        let external = read(&self.root, path)?;
        let mut hashes = Vec::new();
        for text in [expected, external.as_str()] {
            let scratch = self.root.join("hash-input");
            fs::write(&scratch, text)?;
            let hash = process(
                &self.root,
                "/usr/bin/shasum",
                &["-a", "256", scratch.to_str().unwrap()],
            )?;
            fs::remove_file(scratch)?;
            hashes.push(
                hash.split_whitespace()
                    .next()
                    .context("missing SHA256")?
                    .to_owned(),
            );
        }
        self.event(
            case,
            path.file_name().unwrap().to_str().unwrap(),
            hashes[1].clone(),
            hashes[0].clone(),
        )
    }
}

#[test]
#[ignore = "requires exact opt-in, new temporary output directory, real macOS AX/TCC and TextEdit"]
fn native_context_pause_continue_matrix() -> Result<()> {
    run_native_matrix(false)
}

#[test]
#[ignore = "requires startup continuation opt-in, owned TextEdit, AX/TCC and exclusive focus/clipboard"]
fn native_context_pause_continue_service_composition() -> Result<()> {
    ensure!(
        std::env::var("VOICETEXT_EL_PAUSE_CONTINUE_V1").as_deref() == Ok("true"),
        "continuation opt-in must be true before process startup"
    );
    run_native_matrix(true)
}

static NATIVE_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
static NEXT_NATIVE_RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn run_native_matrix(composed: bool) -> Result<()> {
    let _serial = NATIVE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    ensure!(
        std::env::var("VOICETEXT_NATIVE_CONTEXT_E2E").as_deref() == Ok("OWNED_TEXTEDIT_ONLY"),
        "explicit opt-in required"
    );
    let requested = PathBuf::from(std::env::var("VOICETEXT_NATIVE_CONTEXT_OUTPUT")?);
    let temp = std::env::temp_dir().canonicalize()?;
    ensure!(
        requested.parent() == Some(temp.as_path()),
        "output must be directly under canonical system temp directory"
    );
    let name = requested
        .file_name()
        .context("output name")?
        .to_str()
        .context("UTF8 path")?;
    ensure!(
        name.starts_with("native-context-")
            && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "safe new directory name required"
    );
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&requested)
        .context("output MUST NOT preexist")?;
    let root = requested.canonicalize()?;
    let _pool = Pool(unsafe { msg_send![class!(NSAutoreleasePool), new] });
    ensure!(root == requested, "canonical output required");
    let mut evidence = Evidence {
        root: root.clone(),
        file: OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(root.join("results.jsonl"))?,
    };
    let paths = [root.join("owned-a.txt"), root.join("owned-b.txt")];
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let manager = ContinuationContextManager::new();
    let native_names = [
        "own-utf16-dedup",
        "same-pid-document",
        "caret",
        "selection",
        "local-edit",
        "clipboard-refusal",
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        "clipboard-restore-race",
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        "post-paste-readback-unavailable",
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        "e57-post-paste-readback-mismatch",
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        "e35-unsupported-field",
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        "e35-revoked-trust",
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        "e35-secure-field",
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        "e35-expired-budget",
    ];
    let composition_names = [
        "e23_actual_target_change_cold_b_rejects_late_a",
        "e62_actual_target_change_after_preliminary_guard_restores_before_b_write",
        "e62_unconfirmed_release_refuses_same_cold_b_token",
    ];
    let names: &[&str] = if composed {
        &composition_names
    } else {
        &native_names
    };
    let mut clipboard = None;
    let mut created = Vec::new();
    let mut failed = false;
    let mut reporting_failed = false;
    let setup = (|| -> Result<()> {
        clipboard = Some(Clipboard::snapshot()?);
        for p in &paths {
            let mut f = OpenOptions::new().create_new(true).write(true).open(p)?;
            f.write_all(b"seed ")?;
            created.push(p.clone());
            focus(&root, p)?;
            evidence.text("setup", p, "seed ")?;
        }
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        if !composed {
            use app_lib::infrastructure::auto_paste::SyntheticTextEditReader;
            let path = root.join("owned-reader.txt");
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?;
            created.push(path.clone());
            let target = focus(&root, &path)?;
            let reader = bind_synthetic_reader(&path)?;
            let start = SyntheticTextEditReader::clock_ms();
            ensure!(
                read_synthetic_reader(&reader)?.as_deref() == Some(""),
                "reader initial empty"
            );
            writeln!(
                evidence.file,
                "{}",
                json!({"stage":"reader-empty","readStartMs":start,
                "readEndMs":SyntheticTextEditReader::clock_ms(),"identity":reader.identity_evidence()})
            )?;
            // Synthetic document setup only: legacy paste can mutate the clipboard
            // without an ownership receipt. Production legacy insertion is tested separately.
            let expected = "Reader 🦀 synthetic AppleScript insertion";
            script(
                &root,
                &format!(
                    "tell application \"TextEdit\" to set text of ({}) to {}",
                    doc(&path),
                    quoted(expected)
                ),
            )?;
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let start = SyntheticTextEditReader::clock_ms();
                let text = read_synthetic_reader(&reader)?;
                writeln!(
                    evidence.file,
                    "{}",
                    json!({"stage":"reader-synthetic-applescript","readStartMs":start,
                    "readEndMs":SyntheticTextEditReader::clock_ms(),"text":text})
                )?;
                if text.as_deref() == Some(expected) {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "reader exact synthetic AppleScript insertion missing"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            evidence.text("reader-synthetic-applescript-exact", &path, expected)?;
            script(
                &root,
                &format!(
                    "tell application \"TextEdit\" to set text of ({}) to {}",
                    doc(&path),
                    quoted(&"x".repeat(4097))
                ),
            )?;
            ensure!(
                read_synthetic_reader(&reader)
                    .unwrap_err()
                    .to_string()
                    .contains("overflow"),
                "reader must reject full AX UTF16 overflow"
            );
            writeln!(
                evidence.file,
                "{}",
                json!({"stage":"reader-overflow-refused","pass":true})
            )?;
            ensure!(
                focus(&root, &paths[0])? == target,
                "reader same PID precondition"
            );
            ensure!(
                reader
                    .read(None, Instant::now() + Duration::from_millis(200))
                    .unwrap_err()
                    .to_string()
                    .contains("owned editor/window changed"),
                "reader must refuse other document in same PID before reading its text"
            );
            writeln!(
                evidence.file,
                "{}",
                json!({"stage":"reader-same-pid-refused","pass":true})
            )?;
        }
        Ok(())
    })();
    let ready = setup.is_ok();
    if writeln!(
        evidence.file,
        "{}",
        json!({"stage":"setup","pass":ready,"error":setup.err().map(|e|e.to_string())})
    )
    .is_err()
    {
        failed = true;
        reporting_failed = true;
    }
    for (index, case) in names.iter().enumerate() {
        let run = NEXT_NATIVE_RUN.fetch_add(2, std::sync::atomic::Ordering::SeqCst);
        // Sequential process-wide IDs: composition also owns run + 1.
        // A composition owns its Tokio runtime, including unretained async tasks.
        let case_runtime = if composed || index >= 9 {
            Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?,
            )
        } else {
            None
        };
        let result = if ready && !reporting_failed {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                case_runtime.as_ref().unwrap_or(&rt).block_on(async {
                    let clip = clipboard.as_mut().context("clipboard setup")?;
                    if composed {
                        return composition(case, run, index != 0, index == 2,
                            &manager, clip, &mut evidence, &paths).await;
                    }
                    clip.restore()?;
                    let marker = format!("{}-case-{run}", root.file_name().unwrap().to_str().unwrap());
                    clip.write(&marker)?;
                    let target = focus(&root, &paths[0])?;
                    writeln!(
                        evidence.file,
                        "{}",
                        json!({"case":case,"run":run,"target_pid":target.pid,"bundle_id":target.bundle_id})
                    )?;
                    #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                    if index >= 9 {
                        use app_lib::infrastructure::continuation_context::native_e2e::CaptureFault;
                        let fault = [CaptureFault::Unsupported, CaptureFault::Revoked,
                            CaptureFault::Secure, CaptureFault::Timeout][index - 9];
                        return capture_refusal(run, target, fault, &manager, clip).await;
                    }
                    let before = read(&root, &paths[0])?;
                    let other = read(&root, &paths[1])?;
                    range(&target, &paths[0], before.encode_utf16().count() as isize, 0)?;
                    let captured = bounded(manager.capture(run, Some(target.clone()), true)).await?;
                    evidence.event(
                        case,
                        "capture",
                        format!("{captured:?}"),
                        format!("{:?}", V::Valid { revision: 0 }),
                    )?;
                    let mut expected = before.clone();
                    if index == 0 {
                        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                        let os_reader = bind_synthetic_reader(&paths[0])?;
                        for (seq, text, rev) in [(1, "😀e\u{301}", 1), (1, "😀e\u{301}", 1), (2, "𐐀!", 2)]
                        {
                            editor(&target, &paths[0])?;
                            clip.check()?;
                            let before_clip = clip.revision;
                            // Timeout/panic/post-effect failures retain uncertainty.
                            clip.uncertain = true;
                            let started = std::time::Instant::now();
                            let result = bounded(manager.guarded_paste(run, seq, text.into())).await;
                            if let Ok(outcome) = &result { clip.refused(before_clip, outcome); }
                            writeln!(evidence.file, "{}", json!({"case":case,"stage":"paste-result","before_revision":before_clip,"after_revision":clip.count(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"outcome":format!("{result:?}")}))?;
                            let outcome = result?;
                            if matches!(outcome, P::Confirmed { .. }) {
                                clip.confirmed(before_clip, seq == 1 && expected != before, &marker)?;
                            }
                            evidence.event(
                                case,
                                "paste",
                                format!("{outcome:?}"),
                                format!("{:?}", P::Confirmed { revision: rev }),
                            )?;
                            if expected == before || seq == 2 {
                                expected.push_str(text);
                            }
                            evidence.text(case, &paths[0], &expected)?;
                            #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                            {
                                let start = app_lib::infrastructure::auto_paste::SyntheticTextEditReader::clock_ms();
                                let text = read_synthetic_reader(&os_reader)?;
                                writeln!(evidence.file, "{}", json!({"stage":"reader-guarded","readStartMs":start,
                                    "readEndMs":app_lib::infrastructure::auto_paste::SyntheticTextEditReader::clock_ms(),"text":text}))?;
                                ensure!(
                                    text.as_deref() == Some(expected.as_str()),
                                    "retained OS reader after guarded own paste/dedup"
                                );
                            }
                            let validation = bounded(manager.validate(run)).await?;
                            evidence.event(
                                case,
                                "advanced-baseline",
                                format!("{validation:?}"),
                                format!("{:?}", V::Valid { revision: rev }),
                            )?;
                        }
                    } else if index == 6 {
                        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                        {
                        use app_lib::infrastructure::continuation_context::native_e2e::arm_clipboard_restore_barrier;

                        editor(&target, &paths[0])?;
                        let text = " E37_INSERT";
                        let user_copy = format!("{marker}-USER_DURING_RESTORE");
                        let barrier = arm_clipboard_restore_barrier()?;
                        clip.check()?;
                        clip.uncertain = true;
                        let paste = manager.guarded_paste(run, 1, text.into());
                        let external_write = async {
                            let deadline = Instant::now() + Duration::from_secs(2);
                            loop {
                                if barrier.try_reached()? {
                                    break;
                                }
                                ensure!(
                                    Instant::now() < deadline,
                                    "clipboard restore barrier was not reached"
                                );
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            // A separately owned writer snapshots the current
                            // production publication, then replaces it. It must
                            // not inherit this fixture's deliberately uncertain
                            // ownership or restore the displaced publication.
                            let mut writer = Clipboard::snapshot()?;
                            writer.write(&user_copy)?;
                            let user_revision = writer.count();
                            let user_image = writer.image()?;
                            writer.dirty = false;
                            barrier.resume()?;
                            Ok::<_, anyhow::Error>((user_revision, user_image))
                        };
                        let (outcome, external) = tokio::join!(bounded(paste), external_write);
                        let outcome = outcome?;
                        let (user_revision, user_image) = external?;
                        evidence.event(
                            case,
                            "paste-confirmed-before-restore-race",
                            format!("{outcome:?}"),
                            format!("{:?}", P::Confirmed { revision: 1 }),
                        )?;
                        ensure!(
                            clip.count() == user_revision && clip.image()? == user_image,
                            "production restore overwrote concurrent user clipboard"
                        );
                        ensure!(
                            user_image
                                == vec![vec![(
                                    "public.utf8-plain-text".into(),
                                    user_copy.as_bytes().to_vec()
                                )]],
                            "concurrent user clipboard bytes changed"
                        );
                        clip.revision = user_revision;
                        clip.uncertain = false;
                        expected.push_str(text);
                        evidence.text(case, &paths[0], &expected)?;

                        let duplicate_count = clip.count();
                        let duplicate =
                            bounded(manager.guarded_paste(run, 1, text.into())).await?;
                        evidence.event(
                            case,
                            "duplicate-does-not-retry",
                            format!("{duplicate:?}"),
                            format!("{:?}", P::Confirmed { revision: 1 }),
                        )?;
                        ensure!(
                            clip.count() == duplicate_count && clip.image()? == user_image,
                            "duplicate delivery touched concurrent user clipboard"
                        );
                        evidence.event(
                            case,
                            "advanced-baseline",
                            format!("{:?}", bounded(manager.validate(run)).await?),
                            format!("{:?}", V::Valid { revision: 1 }),
                        )?;
                        }
                        #[cfg(not(all(debug_assertions, feature = "native-window-e2e")))]
                        unreachable!("native E37 case is feature-gated");
                    } else if index == 7 || index == 8 {
                        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
                        {
                            use app_lib::infrastructure::continuation_context::native_e2e::{arm_post_paste_readback_unavailable, arm_post_paste_readback_mismatch};

                            editor(&target, &paths[0])?;
                            let known = " E38_KNOWN";
                            clip.check()?;
                            let known_before = clip.revision;
                            clip.uncertain = true;
                            let known_outcome = bounded(
                                manager.guarded_paste(run, 1, known.into()),
                            )
                            .await?;
                            clip.refused(known_before, &known_outcome);
                            ensure!(
                                matches!(known_outcome, P::Confirmed { revision: 1 }),
                                "known prefix was not confirmed: {known_outcome:?}"
                            );
                            clip.confirmed(known_before, false, &marker)?;
                            expected.push_str(known);
                            evidence.text(case, &paths[0], &expected)?;

                            let uncertain = if index == 8 { " E57_e\u{301}" } else { " E38_UNKNOWN" };
                            let fault = if index == 8 {
                                arm_post_paste_readback_mismatch(run, 2)?
                            } else {
                                arm_post_paste_readback_unavailable(run, 2)?
                            };
                            clip.check()?;
                            let uncertain_before = clip.revision;
                            clip.uncertain = true;
                            let uncertain_outcome = bounded(
                                manager.guarded_paste(run, 2, uncertain.into()),
                            )
                            .await?;
                            ensure!(
                                fault.was_consumed(),
                                "run/sequence-scoped post-paste fault was not consumed"
                            );
                            ensure!(
                                matches!(uncertain_outcome, P::Uncertain),
                                "post-paste readback fault was not uncertain: {uncertain_outcome:?}"
                            );
                            let uncertain_revision = uncertain_before
                                .checked_add(1)
                                .context("clipboard revision overflow")?;
                            let uncertain_image = vec![vec![(
                                "public.utf8-plain-text".into(),
                                uncertain.as_bytes().to_vec(),
                            )]];
                            ensure!(
                                clip.count() == uncertain_revision
                                    && clip.image()? == uncertain_image,
                                "uncertain insertion did not retain its exact publication receipt"
                            );
                            // Adopt only the exact fixture-owned publication so teardown can
                            // restore the original clipboard without treating text equality as
                            // proof of ownership.
                            clip.revision = uncertain_revision;
                            clip.uncertain = false;
                            expected.push_str(uncertain);
                            evidence.text(case, &paths[0], &expected)?;
                            evidence.text(case, &paths[1], &other)?;

                            let before_refusals = clip.count();
                            for (seq, text, stage) in [
                                (2, uncertain, "duplicate-uncertain-refused"),
                                (3, " MUST_NOT_INSERT", "later-delivery-refused"),
                            ] {
                                let outcome = bounded(
                                    manager.guarded_paste(run, seq, text.into()),
                                )
                                .await?;
                                evidence.event(
                                    case,
                                    stage,
                                    format!("{outcome:?}"),
                                    "Unavailable".into(),
                                )?;
                                ensure!(
                                    matches!(outcome, P::Unavailable),
                                    "uncertain run accepted another delivery: {outcome:?}"
                                );
                            }
                            ensure!(
                                clip.count() == before_refusals
                                    && clip.image()? == uncertain_image,
                                "refused delivery touched uncertain clipboard publication"
                            );
                            let (observations, overflow) =
                                app_lib::infrastructure::continuation_context::native_e2e::delivery_observations(run);
                            ensure!(!overflow, "native insertion observation overflow");
                            let attempted: Vec<_> = observations
                                .iter()
                                .filter(|record| record.insertion_started)
                                .collect();
                            ensure!(
                                attempted.len() == 2
                                    && attempted[0].delivery_seq == 1
                                    && attempted[0].insertion_finished
                                    && attempted[0].insertion_confirmed
                                    && matches!(attempted[0].result, Some(P::Confirmed { revision: 1 }))
                                    && attempted[1].delivery_seq == 2
                                    && attempted[1].insertion_finished
                                    && !attempted[1].insertion_confirmed
                                    && matches!(attempted[1].result, Some(P::Uncertain)),
                                "native insertion trace did not contain one confirmed and one uncertain effect: {observations:?}"
                            );
                            ensure!(
                                observations.len() == 4
                                    && observations[2..].iter().all(|record| {
                                        !record.insertion_started
                                            && matches!(record.result, Some(P::Unavailable))
                                    }),
                                "retired run attempted another native effect: {observations:?}"
                            );
                            evidence.text(case, &paths[0], &expected)?;
                            evidence.text(case, &paths[1], &other)?;
                        }
                        #[cfg(not(all(debug_assertions, feature = "native-window-e2e")))]
                        unreachable!("native E38 case is feature-gated");
                    } else {
                        match index {
                            1 | 5 => {
                                let second = focus(&root, &paths[1])?;
                                ensure!(second == target, "same TextEdit PID required");
                            }
                            2 => range(&target, &paths[0], 0, 0)?,
                            3 => range(&target, &paths[0], 0, 1)?,
                            4 => {
                                editor(&target, &paths[0])?;
                                expected.push_str("LOCAL_EDIT");
                                script(
                                    &root,
                                    &format!(
                                        "tell application \"TextEdit\" to set text of ({}) to {}",
                                        doc(&paths[0]),
                                        quoted(&expected)
                                    ),
                                )?;
                                range(&target, &paths[0], before.encode_utf16().count() as isize, 0)?;
                            }
                            _ => unreachable!(),
                        }
                        clip.check()?;
                        if index == 5 {
                            clip.write(&format!("{marker}-USER_COPY"))?;
                        }
                        let count = clip.count();
                        if index == 5 {
                            clip.uncertain = true;
                            let started = std::time::Instant::now();
                            let result = bounded(manager.guarded_paste(run, 1, "MUST_NOT_INSERT".into())).await;
                            if let Ok(outcome) = &result { clip.refused(count, outcome); }
                            writeln!(evidence.file, "{}", json!({"case":case,"stage":"paste-result","before_revision":count,"after_revision":clip.count(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"outcome":format!("{result:?}")}))?;
                            let outcome = result?;
                            evidence.event(
                                case,
                                "direct-native-refusal",
                                format!("{outcome:?}"),
                                "ContextMismatch".into(),
                            )?;
                            ensure!(clip.count() == count, "refusal changed clipboard ownership");
                        }
                        let validation = bounded(manager.validate(run)).await?;
                        evidence.event(
                            case,
                            "native-validation",
                            format!("{validation:?}"),
                            "Mismatch".into(),
                        )?;
                        // validate(Mismatch) sticky-retires registration; following paste is exactly Unavailable.
                        clip.uncertain = true;
                        let started = std::time::Instant::now();
                        let result = bounded(manager.guarded_paste(run, 1, "MUST_NOT_INSERT".into())).await;
                        if let Ok(outcome) = &result { clip.refused(count, outcome); }
                        writeln!(evidence.file, "{}", json!({"case":case,"stage":"paste-result","before_revision":count,"after_revision":clip.count(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"outcome":format!("{result:?}")}))?;
                        let outcome = result?;
                        evidence.event(
                            case,
                            "refused-paste",
                            format!("{outcome:?}"),
                            "Unavailable".into(),
                        )?;
                        ensure!(clip.count() == count, "refusal changed clipboard ownership");
                        evidence.event(
                            case,
                            "clipboard-changeCount",
                            clip.count().to_string(),
                            count.to_string(),
                        )?;
                        if index == 5 {
                            ensure!(
                                clip.image()? == vec![vec![("public.utf8-plain-text".into(),
                                    format!("{marker}-USER_COPY").into_bytes())]],
                                "user-style clipboard mutation lost"
                            );
                        }
                    }
                    evidence.text(case, &paths[0], &expected)?;
                    evidence.text(case, &paths[1], &other)?;
                    Ok::<_, anyhow::Error>(())
                })
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("case panicked; cleanup follows")))
        } else {
            Err(anyhow::anyhow!(
                "required case not executed: setup or evidence failed"
            ))
        };
        if let Some(runtime) = case_runtime {
            // Cancel remaining async tasks before fixture release/reuse. This does
            // not prove joined detached tasks, blocking work, or native shutdown.
            runtime.shutdown_timeout(Duration::from_secs(4));
        }
        let released = rt.block_on(bounded(manager.release(run)));
        let released_b = if composed {
            rt.block_on(bounded(manager.release(run + 1)))
        } else {
            Ok(())
        };
        let released = released.and(released_b);
        let restored = clipboard.as_mut().map(Clipboard::restore).transpose();
        if (composed || index < 9) && (result.is_err() || released.is_err() || restored.is_err()) {
            for path in &created {
                let captured = (|| -> Result<()> {
                    let text = read(&root, path)?;
                    let filename = path
                        .file_name()
                        .context("owned filename")?
                        .to_str()
                        .context("UTF8 filename")?;
                    fs::write(
                        root.join(format!("{case}-{filename}.readback")),
                        text.as_bytes(),
                    )?;
                    evidence.text(case, path, &text)
                })();
                if writeln!(evidence.file, "{}", json!({"case":case,"stage":"failure-readback","file":path.file_name().and_then(|s|s.to_str()),"error":captured.err().map(|e|e.to_string())})).is_err() {
                    reporting_failed = true;
                }
            }
        }
        let pass = result.is_ok() && released.is_ok() && restored.is_ok();
        failed |= !pass;
        if writeln!(
            evidence.file,
            "{}",
            json!({"case":case,"pass":pass,"error":result.err().map(|e|format!("{e:#}")),"release_error":released.err().map(|e|e.to_string()),"clipboard_cleanup_error":restored.err().map(|e|e.to_string())})
        ).and_then(|_| evidence.file.flush()).is_err() {
            failed = true;
            reporting_failed = true;
        }
    }
    for path in created {
        let closed = script(&root, &format!("tell application \"TextEdit\"\nrepeat with d in (every document whose path is {})\nclose d saving no\nend repeat\nend tell", quoted(path.to_str().unwrap())));
        failed |= closed.is_err();
        if writeln!(
            evidence.file,
            "{}",
            json!({"stage":"close-owned-document","file":path.file_name().and_then(|s|s.to_str()),"pass":closed.is_ok()})
        ).is_err() {
            failed = true;
        }
    }
    failed |= evidence.file.flush().is_err();
    ensure!(
        !failed,
        "required native matrix failed; inspect {}/results.jsonl; owned files retained, TextEdit never quit",
        root.display()
    );
    Ok(())
}

// Bounded native/service composition, intentionally not a Pinia/Tauri IPC test.
use app_lib::application::services::{
    ContinuationRefusal, ContinueCaptureOutcome, PreparedCaptureToken, TranscriptionService,
};
use app_lib::domain::{
    AppConfig, AudioCapture, AudioChunk, AudioChunkCallback, AudioConfig, AudioResult,
    BackendStreamingProvider, ProviderRelease, SttConfig, SttProvider, SttProviderFactory,
    SttProviderType, SttResult, TranscriptionCallback,
};
use app_lib::infrastructure::stt::BackendProvider;
use async_trait::async_trait;
use futures_util::{FutureExt, SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

type Source = Arc<Mutex<Option<AudioChunkCallback>>>;
struct DrivenCapture {
    config: AudioConfig,
    source: Source,
}
#[async_trait]
impl AudioCapture for DrivenCapture {
    async fn initialize(&mut self, config: AudioConfig) -> AudioResult<()> {
        self.config = config;
        Ok(())
    }
    async fn start_capture(&mut self, callback: AudioChunkCallback) -> AudioResult<()> {
        *self.source.lock().unwrap() = Some(callback);
        Ok(())
    }
    async fn stop_capture(&mut self) -> AudioResult<()> {
        *self.source.lock().unwrap() = None;
        Ok(())
    }
    fn is_capturing(&self) -> bool {
        self.source.lock().unwrap().is_some()
    }
    fn config(&self) -> AudioConfig {
        self.config
    }
}
fn inject(source: &Source, sample: i16) -> Result<()> {
    let callback = source
        .lock()
        .unwrap()
        .clone()
        .context("capture callback missing")?;
    callback(AudioChunk::new(vec![sample; 480], 16000, 1));
    Ok(())
}
struct LocalBackend;
impl SttProviderFactory for LocalBackend {
    fn create(&self, _: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
        Ok(Box::new(BackendProvider::new()))
    }
}
struct ObservedGuard {
    manager: ContinuationContextManager,
    calls: Mutex<Vec<(u64, V)>>,
}
#[async_trait]
impl ContinuationContextGuard for ObservedGuard {
    async fn validate(&self, run: u64) -> V {
        let actual = self.manager.validate(run).await;
        self.calls.lock().unwrap().push((run, actual));
        actual
    }
}
#[derive(Debug)]
struct Delivery {
    run: u64,
    seq: Option<u64>,
    text: String,
}
fn delivery_callback(
    run: u64,
    tx: mpsc::Sender<Delivery>,
    overflow: Arc<AtomicBool>,
) -> TranscriptionCallback {
    Arc::new(move |t| {
        if tx
            .try_send(Delivery {
                run,
                seq: t.delivery_seq,
                text: t.text,
            })
            .is_err()
        {
            overflow.store(true, Ordering::Release);
        }
    })
}
async fn connect_local(
    service: &TranscriptionService,
    token: PreparedCaptureToken,
    tx: &mpsc::Sender<Delivery>,
    overflow: &Arc<AtomicBool>,
) -> Result<()> {
    service
        .connect_prepared_recording(
            token,
            Arc::new(|_| {}),
            delivery_callback(token.run_id, tx.clone(), overflow.clone()),
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(|_| {}),
            Arc::new(|_, _| {}),
            Arc::new(AtomicBool::new(false)),
        )
        .await
}
async fn owned_delivery(
    manager: &ContinuationContextManager,
    clip: &mut Clipboard,
    marker: &str,
    delivery: Delivery,
    expected: P,
    evidence: &mut Evidence,
    case: &str,
) -> Result<()> {
    let seq = delivery
        .seq
        .context("real stable callback omitted delivery identity")?;
    clip.check()?;
    let before = clip.revision;
    clip.uncertain = true;
    let result = bounded(manager.guarded_paste(delivery.run, seq, delivery.text)).await;
    if let Ok(outcome) = &result {
        clip.refused(before, outcome);
    }
    let outcome = result?;
    if matches!(outcome, P::Confirmed { .. }) {
        clip.confirmed(before, false, marker)?;
    }
    evidence.event(
        case,
        "owned-delivery",
        format!("{outcome:?}"),
        format!("{expected:?}"),
    )?;
    writeln!(
        evidence.file,
        "{}",
        json!({"case":case,"run":delivery.run,"delivery_seq":seq,
        "before_revision":before,"after_revision":clip.count()})
    )?;
    clip.check()
}

#[derive(Default)]
struct Wire {
    pcm: [Vec<u8>; 2],
    controls: Vec<serde_json::Value>,
    configs: usize,
    connections: usize,
    pongs: usize,
}
// One listener remains live through the refused cold attempt, so a forbidden
// second connection cannot disappear behind a closed listener.
async fn local_peer(
    listener: tokio::net::TcpListener,
    wire: Arc<Mutex<Wire>>,
    pong: mpsc::Sender<()>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
    unconfirmed: bool,
) -> Result<()> {
    for connection in 0..2 {
        let socket = tokio::select! {
            _ = &mut shutdown => return Ok(()),
            accepted = tokio::time::timeout(Duration::from_secs(12), listener.accept()) => accepted??.0,
        };
        wire.lock().unwrap().connections += 1;
        let mut ws = bounded(tokio_tungstenite::accept_async(socket)).await??;
        let config = bounded(ws.next()).await?.context("Config EOF")??;
        let Message::Text(config) = config else {
            bail!("Config must be text")
        };
        let config: serde_json::Value = serde_json::from_str(&config)?;
        ensure!(
            config["type"] == "config"
                && config["capabilities"]
                    .as_array()
                    .context("capabilities")?
                    .contains(&json!("el_pause_continue_v1")),
            "startup continuation opt-in missing"
        );
        wire.lock().unwrap().configs += 1;
        let session = if connection == 0 {
            "owned-A"
        } else {
            "owned-B"
        };
        ws.send(Message::Text(
            json!({"type":"ready","session_id":session,
            "accepted_capabilities":["finalize_outcome_v1","el_pause_continue_v1"]})
            .to_string()
            .into(),
        ))
        .await?;
        let mut seq = 0;
        let mut finalized = false;
        let mut continued = None;
        loop {
            let message = tokio::select! {
                _ = &mut shutdown => { let _ = bounded(ws.close(None)).await; return Ok(()); },
                message = tokio::time::timeout(Duration::from_secs(12), ws.next()) => message?.context("peer EOF before Close")??,
            };
            match message {
                Message::Binary(bytes) => {
                    ensure!(!finalized, "PCM after finalization");
                    let expected = if connection == 0 { 1200_i16 } else { 2400_i16 }
                        .to_le_bytes()
                        .repeat(480);
                    let complete = {
                        let mut log = wire.lock().unwrap();
                        log.pcm[connection].extend_from_slice(&bytes);
                        ensure!(
                            expected.starts_with(&log.pcm[connection]),
                            "wrong/duplicate PCM on connection {connection}"
                        );
                        expected == log.pcm[connection]
                    };
                    seq += 1;
                    ws.send(Message::Text(
                        json!({"type":"ack","seq":seq}).to_string().into(),
                    ))
                    .await?;
                    if complete {
                        ws.send(Message::Text(
                            json!({"type":"stable","delivery_seq":1,
                            "text":if connection == 0 {"A-first"} else {"B-only"}})
                            .to_string()
                            .into(),
                        ))
                        .await?;
                    }
                }
                Message::Text(text) => {
                    let v: serde_json::Value = serde_json::from_str(&text)?;
                    let kind = v["type"].as_str().context("control type")?;
                    if kind != "keepalive" {
                        let mut log = wire.lock().unwrap();
                        ensure!(log.controls.len() < 12, "control transcript exceeded bound");
                        log.controls.push(v.clone());
                    }
                    match kind {
                        "pause" | "continue" | "pause_restore" => {
                            ensure!(connection == 0 && !finalized, "control on wrong owner");
                            if kind == "continue" {
                                ensure!(continued.is_none(), "duplicate Continue");
                                continued = Some(v["request_id"].clone());
                            }
                            if kind != "pause" {
                                ensure!(v["pause_epoch"] == 7, "epoch mismatch");
                            }
                            if kind == "pause_restore" {
                                ensure!(
                                    Some(&v["continue_request_id"]) == continued.as_ref(),
                                    "Restore correlation mismatch"
                                );
                            }
                            ws.send(Message::Text(json!({"type":match kind {"pause"=>"pause_accepted","continue"=>"continue_result",_=>"pause_restore_result"},
                                "request_id":v["request_id"],"provider_session_id":session,"pause_epoch":7,
                                "decision":"accepted","eligible_now":true,"reason":null,"continue_window_ms":2000,
                                "current_phase":if kind == "continue" {"active_awaiting_audio"} else {"paused_reclaimable"}}).to_string().into())).await?;
                            if kind == "continue" {
                                ws.send(Message::Ping(b"accepted-reader".to_vec().into()))
                                    .await?;
                            }
                        }
                        "finalize" => {
                            ensure!(!finalized, "duplicate finalization");
                            finalized = true;
                            if connection == 0 {
                                ws.send(Message::Text(
                                    json!({"type":"stable","delivery_seq":2,"text":"A-late"})
                                        .to_string()
                                        .into(),
                                ))
                                .await?;
                            }
                            ws.send(Message::Text(json!({"type":"finalize_complete","status":"drained","saw_result":true,
                                "outcome":{"reason":"drained","tail_evidence":"unconfirmed",
                                "provider_release":if unconfirmed && connection == 0 {"unconfirmed"} else {"released"},
                                "last_delivery_seq":if connection == 0 {2} else {1},
                                "stable_snapshot":if connection == 0 {"A-first A-late"} else {"B-only"}}}).to_string().into())).await?;
                        }
                        "close" => ensure!(finalized, "close before finalize"),
                        "keepalive" => {}
                        _ => bail!("unexpected control {kind}"),
                    }
                }
                Message::Pong(bytes) => {
                    ensure!(bytes.as_slice() == b"accepted-reader", "unexpected Pong");
                    wire.lock().unwrap().pongs += 1;
                    pong.try_send(())?;
                }
                Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await?,
                Message::Close(_) => {
                    ensure!(finalized, "transport closed before finalization");
                    break;
                }
                _ => bail!("unexpected peer frame"),
            }
        }
    }
    Ok(())
}

async fn composition(
    case: &str,
    run: u64,
    after_accepted: bool,
    unconfirmed: bool,
    manager: &ContinuationContextManager,
    clip: &mut Clipboard,
    evidence: &mut Evidence,
    paths: &[PathBuf; 2],
) -> Result<()> {
    ensure!(
        std::env::var("VOICETEXT_EL_PAUSE_CONTINUE_V1").as_deref() == Ok("true"),
        "set VOICETEXT_EL_PAUSE_CONTINUE_V1=true BEFORE process startup (OnceLock)"
    );
    let root = evidence.root.clone();
    let source: Source = Arc::new(Mutex::new(None));
    let service = TranscriptionService::new(
        Box::new(DrivenCapture {
            config: AudioConfig::default(),
            source: source.clone(),
        }),
        Arc::new(LocalBackend),
    );
    service.set_effective_capture_device(Some("owned-manual-pcm".into()));
    let guard = Arc::new(ObservedGuard {
        manager: manager.clone(),
        calls: Mutex::new(Vec::new()),
    });
    service.set_continuation_context_guard(guard.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let mut config = SttConfig::new(SttProviderType::Backend);
    config.backend_url = Some(format!("ws://{}", listener.local_addr()?));
    config.backend_auth_token = Some("synthetic-owned-local".into());
    config.backend_streaming_provider = BackendStreamingProvider::ElevenLabs;
    let wire = Arc::new(Mutex::new(Wire::default()));
    let (pong_tx, mut pong_rx) = mpsc::channel(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let mut peer = tokio::spawn(local_peer(
        listener,
        wire.clone(),
        pong_tx,
        shutdown_rx,
        unconfirmed,
    ));
    let (tx, mut rx) = mpsc::channel(4);
    let overflow = Arc::new(AtomicBool::new(false));
    let mut retained = None;
    // Own the SAME pinned operation outside both observation deadlines.
    // Clear only after Ready, before propagating the operation result.
    let mut finalization = None;
    // Catch inside the ownership scope; cleanup runs on failures and panics.
    let result = std::panic::AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(30), async {
        clip.restore()?;
        let marker = format!("{}-{case}", root.file_name().unwrap().to_str().unwrap());
        clip.write(&marker)?;
        for path in paths {
            script(&root, &format!("tell application \"TextEdit\" to set text of ({}) to \"seed \"", doc(path)))?;
        }
        let target = focus(&root, &paths[0])?;
        range(&target, &paths[0], 5, 0)?;
        ensure!(bounded(manager.capture(run, Some(target.clone()), true)).await? == V::Valid { revision: 0 }, "A capture");
        #[cfg(all(debug_assertions, feature = "native-window-e2e"))]
        config.qualify_continuation_target_for_native_e2e();
        #[cfg(not(all(debug_assertions, feature = "native-window-e2e")))]
        bail!("service composition requires a debug native-window-e2e build");
        service.update_config(config.clone()).await?;
        let policy = AppConfig::default();
        service.register_continuation_policy(run, &policy).await;
        let a = service.prepare_recording_capture(run, config.clone(), Arc::new(|_, _| {}), Arc::new(|_, _| {}), Arc::new(|_| {})).await?;
        retained = Some(a);
        inject(&source, 1200)?;
        bounded(connect_local(&service, a, &tx, &overflow)).await??;
        retained = None;
        let first = bounded(rx.recv()).await?.context("initial A callback")?;
        ensure!(first.run == run && first.seq == Some(1) && first.text == "A-first", "initial A identity");
        owned_delivery(manager, clip, &marker, first, P::Confirmed { revision: 1 }, evidence, case).await?;
        evidence.text(case, &paths[0], "seed A-first")?;
        service.stop_capture_for_run(run).await?;
        let stopped = Instant::now();
        let paused = service.pause_for_continuation(run, stopped.into()).await?;
        // A registration remains live until after delayed delivery; no release here.
        guard.calls.lock().unwrap().clear();
        if !after_accepted {
            ensure!(focus(&root, &paths[1])? == target, "same PID/bundle required");
        }
        service.register_continuation_policy(run + 1, &policy).await;
        let b = service.prepare_recording_capture(run + 1, config.clone(), Arc::new(|_, _| {}), Arc::new(|_, _| {}), Arc::new(|_| {})).await?;
        retained = Some(b);
        ensure!(service.continuation_policy(run) == service.continuation_policy(run + 1), "equal effective policy required");
        if !after_accepted { inject(&source, 2400)?; }
        let continue_call = service.continue_prepared_capture(b, paused, tokio::time::Instant::now(), Arc::new(AtomicBool::new(false)));
        let outcome = if after_accepted {
            let switch = async {
                bounded(pong_rx.recv()).await?.context("Accepted reader Pong missing")?;
                ensure!(guard.calls.lock().unwrap().as_slice() == [(run, V::Valid { revision: 1 })], "real preliminary guard required");
                ensure!(focus(&root, &paths[1])? == target, "same PID/bundle required");
                editor(&target, &paths[1])?;
                inject(&source, 2400)
            };
            let (outcome, switched) = tokio::join!(continue_call, switch);
            switched?; outcome?
        } else { continue_call.await? };
        ensure!(outcome == ContinueCaptureOutcome::Unsent(ContinuationRefusal::ContextMismatch), "native Continue refusal: {outcome:?}");
        let expected_calls = if after_accepted { vec![(run, V::Valid { revision: 1 }), (run, V::Mismatch)] } else { vec![(run, V::Mismatch)] };
        ensure!(*guard.calls.lock().unwrap() == expected_calls, "delegated guard sequence");
        {
            let log = wire.lock().unwrap();
            let count = |kind: &str| log.controls.iter().filter(|v| v["type"] == kind).count();
            ensure!(count("continue") == usize::from(after_accepted) && count("pause_restore") == usize::from(after_accepted), "Continue/Restore cardinality");
            ensure!(log.pongs == usize::from(after_accepted), "ordered reader barrier");
            ensure!(log.pcm[0] == 1200_i16.to_le_bytes().repeat(480), "B PCM leaked onto A");
        }
        service.seal_prepared_capture(b).await?;
        finalization = Some(Box::pin(service.finalize_provider_for_run(run)));
        let finalized = bounded(finalization.as_mut().unwrap().as_mut()).await?;
        finalization = None;
        if !unconfirmed { finalized?; }
        let late = bounded(rx.recv()).await?.context("late A receipt barrier")?;
        ensure!(late.run == run && late.seq == Some(2) && late.text == "A-late", "late A original identity");
        let report = service.completed_report_for_run(run).await.context("A terminal report")?;
        ensure!(report.provider_release == if unconfirmed { ProviderRelease::Unconfirmed } else { ProviderRelease::Released }, "A release evidence");
        // Receipt above is distinct from release below: same retained B token goes
        // cold only after the actual terminal report, without rewriting its PCM.
        let cold = bounded(connect_local(&service, b, &tx, &overflow)).await?;
        if unconfirmed {
            let cold_error = cold.unwrap_err().to_string();
            // Unconfirmed finalization retains A's logical owner, so the earlier
            // ownership fence must reject B before the continuation-report gate.
            ensure!(cold_error == "Previous logical provider has not released ownership", "cold must refuse on retained ownership fence; actual error: {cold_error}");
            ensure!(service.logical_provider_run_id() == run, "unconfirmed A ownership was retired");
            ensure!(service.retains_prepared_capture(b).await, "refused cold start consumed B receiver");
            let log = wire.lock().unwrap();
            ensure!(log.configs == 1 && log.connections == 1 && log.pcm[1].is_empty(), "forbidden second connection/Config");
            ensure!(log.pcm[0] == 1200_i16.to_le_bytes().repeat(480), "refused B wrote PCM onto A");
        } else {
            cold?; retained = None;
            editor(&target, &paths[1])?;
            range(&target, &paths[1], 5, 0)?;
            ensure!(bounded(manager.capture(run + 1, Some(target.clone()), true)).await? == V::Valid { revision: 0 }, "B native registration");
            let b_delivery = bounded(rx.recv()).await?.context("B stable callback")?;
            ensure!(b_delivery.run == run + 1 && b_delivery.seq == Some(1) && b_delivery.text == "B-only", "cold B identity");
            owned_delivery(manager, clip, &marker, b_delivery, P::Confirmed { revision: 1 }, evidence, case).await?;
        }
        owned_delivery(manager, clip, &marker, late, P::Unavailable, evidence, case).await?;
        evidence.text(case, &paths[0], "seed A-first")?;
        evidence.text(case, &paths[1], if unconfirmed { "seed " } else { "seed B-only" })?;
        if !unconfirmed {
            finalization = Some(Box::pin(service.finalize_provider_for_run(run + 1)));
            let finalized = bounded(finalization.as_mut().unwrap().as_mut()).await?;
            finalization = None;
            finalized?;
        }
        ensure!(!overflow.load(Ordering::Acquire) && rx.try_recv().is_err(), "delivery queue overflow/extra callback");
        let log = wire.lock().unwrap();
        ensure!(log.connections == if unconfirmed {1} else {2}, "cold connection count");
        ensure!(log.configs == log.connections, "every connection has exactly one Config");
        ensure!(log.pcm[1] == if unconfirmed {Vec::new()} else {2400_i16.to_le_bytes().repeat(480)}, "exact cold B PCM");
        writeln!(evidence.file, "{}", json!({"case":case,"stage":"composition","assertions_pass":true,
            "guards":format!("{expected_calls:?}"),"controls":log.controls,"configs":log.configs,"connections":log.connections,
            "pcm_bytes":[log.pcm[0].len(),log.pcm[1].len()],"provider_release":format!("{:?}",report.provider_release),
            "scope":"native/service only; no Pinia or IPC authorization proof"}))?;
        Ok::<_, anyhow::Error>(())
    })).catch_unwind().await
        .map_err(|_| anyhow::anyhow!("composition panic"))
        .and_then(|r| r.map_err(anyhow::Error::from)).and_then(|r| r);
    // Finish the pending operation while peer and fixture remain alive. A fresh
    // cleanup call cannot recover a provider already removed by that operation.
    let teardown = std::panic::AssertUnwindSafe(tokio::time::timeout(
        Duration::from_secs(60),
        async {
            if let Some(pending) = finalization.as_mut() {
                // Ready(Err) also completes ownership; the observation already failed.
                let _ = pending.as_mut().await;
            }
            finalization = None;
            let cancelled = if let Some(token) = retained {
                service.cancel_prepared_capture(token).await
            } else { Ok(()) };
            service.cleanup_runtime_failure("owned composition cleanup").await;
            cancelled
        },
    )).catch_unwind().await
        .map_err(|_| anyhow::anyhow!("composition teardown panic; task cleanup unproven"))
        .and_then(|r| r.map_err(|_| anyhow::anyhow!(
            "composition teardown timeout; pending cleanup incomplete; case runtime shutdown required; task joining unproven"
        )))
        .and_then(|r| r);
    // Only the exact peer handle below is explicitly joined by this harness.
    // Service cleanup does not prove capture-meter/focus tasks or the native
    // singleton joined. Caller shuts down the case runtime before fixture reuse.
    let _ = shutdown_tx.send(());
    let peer_result = match tokio::time::timeout(Duration::from_secs(4), &mut peer).await {
        Ok(joined) => joined.context("peer panic").and_then(|r| r),
        Err(_) => {
            peer.abort();
            let _ = peer.await;
            Err(anyhow::anyhow!("peer shutdown timeout"))
        }
    };
    // Prioritize teardown failure. Preserve both scenario and peer failures so
    // a rejected synthetic protocol handshake is not hidden by a later timeout.
    // Caller releases both native registrations after case runtime shutdown.
    teardown?;
    match (result, peer_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(scenario), Ok(())) => Err(scenario),
        (Ok(()), Err(peer)) => Err(peer),
        (Err(scenario), Err(peer)) => {
            Err(scenario.context(format!("local peer also failed: {peer:#}")))
        }
    }
}

// E35 fault/service composition. No text readback or framework error payloads.
// Real native capture branches receive one fault; no TCC settings are changed.
#[cfg(all(debug_assertions, feature = "native-window-e2e"))]
async fn capture_refusal(
    run: u64,
    target: AutoPasteTarget,
    fault_kind: app_lib::infrastructure::continuation_context::native_e2e::CaptureFault,
    manager: &ContinuationContextManager,
    clip: &mut Clipboard,
) -> Result<()> {
    use app_lib::infrastructure::continuation_context::native_e2e::{
        arm_capture_fault, delivery_observations,
    };
    struct NoProvider(Arc<std::sync::atomic::AtomicUsize>);
    impl SttProviderFactory for NoProvider {
        fn create(&self, _: &SttConfig) -> SttResult<Box<dyn SttProvider>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(app_lib::domain::SttError::Processing(
                "forbidden provider".into(),
            ))
        }
    }
    let providers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let source: Source = Arc::new(Mutex::new(None));
    let service = TranscriptionService::new(
        Box::new(DrivenCapture {
            config: AudioConfig::default(),
            source: source.clone(),
        }),
        Arc::new(NoProvider(providers.clone())),
    );
    let config = SttConfig::new(SttProviderType::Backend);
    let token = service
        .prepare_recording_capture(
            run,
            config,
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
            Arc::new(|_| {}),
        )
        .await?;
    let result = std::panic::AssertUnwindSafe(async {
        // Enter the actual prepared-service callback before and after refusal.
        // This is readiness evidence; stopped-AX fixture supplies overlap timing.
        inject(&source, 0)?;
        let fault = arm_capture_fault(target.clone(), fault_kind)?;
        let started = Instant::now();
        ensure!(
            bounded(manager.capture(run, Some(target), true)).await? == V::Unavailable,
            "capture must fail closed"
        );
        ensure!(
            started.elapsed() < Duration::from_millis(500),
            "capture refusal exceeded fixture bound"
        );
        ensure!(fault.was_consumed(), "target-scoped fault not consumed");
        ensure!(
            service.retains_prepared_capture(token).await,
            "native refusal consumed prepared capture"
        );
        let callback_start = Instant::now();
        inject(&source, 0)?;
        ensure!(
            callback_start.elapsed() < Duration::from_millis(100),
            "prepared callback blocked"
        );
        let count = clip.count();
        ensure!(
            bounded(manager.validate(run)).await? == V::Unavailable,
            "failed capture revived"
        );
        ensure!(
            bounded(manager.guarded_paste(run, 1, "OWNED_SENTINEL".into())).await?
                == P::Unavailable,
            "failed capture allowed insertion"
        );
        let (records, overflow) = delivery_observations(run);
        ensure!(
            !overflow && records.len() == 1 && !records[0].insertion_started,
            "failed capture entered insertion boundary"
        );
        ensure!(clip.count() == count, "failed capture touched clipboard");
        ensure!(
            providers.load(Ordering::SeqCst) == 0,
            "provider was constructed"
        );
        Ok::<_, anyhow::Error>(())
    })
    .catch_unwind()
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("capture refusal panic")));
    // Cleanup precedes propagation; outer case runtime is then shut down.
    let cleanup = bounded(service.cancel_prepared_capture(token)).await;
    service
        .cleanup_runtime_failure("owned capture refusal cleanup")
        .await;
    cleanup??;
    ensure!(
        source.lock().unwrap().is_none(),
        "synthetic capture remained active"
    );
    result
}
