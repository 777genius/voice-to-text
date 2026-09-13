// References never leave this dedicated probe thread. No content attributes are read.
use anyhow::{ensure, Result};
use std::{
    ffi::{c_void, CStr},
    sync::mpsc,
    time::{Duration, Instant},
};
type Ref = *const c_void;
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, seconds: f32) -> i32;
    fn AXUIElementCopyAttributeValue(element: Ref, key: Ref, value: *mut Ref) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFStringCreateWithBytes(a: Ref, b: *const u8, n: isize, enc: u32, ext: bool) -> Ref;
}
extern "C" {
    static mach_task_self_: u32;
    fn task_threads(task: u32, threads: *mut *mut u32, count: *mut u32) -> i32;
    fn mach_port_deallocate(task: u32, port: u32) -> i32;
    fn vm_deallocate(task: u32, address: usize, size: usize) -> i32;
    fn thread_info(thread: u32, flavor: i32, info: *mut u32, count: *mut u32) -> i32;
}
struct Owned(Ref);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}
#[repr(C)]
struct Extended {
    times: [u64; 2],
    scheduling: [i32; 8],
    name: [i8; 64],
}
struct Threads {
    ports: *mut u32,
    count: u32,
}
impl Drop for Threads {
    fn drop(&mut self) {
        unsafe {
            for i in 0..self.count {
                mach_port_deallocate(mach_task_self_, *self.ports.add(i as usize));
            }
            vm_deallocate(
                mach_task_self_,
                self.ports as usize,
                self.count as usize * 4,
            );
        }
    }
}
pub fn threads() -> Result<(u64, u32)> {
    unsafe {
        let mut sample = Threads {
            ports: std::ptr::null_mut(),
            count: 0,
        };
        ensure!(
            task_threads(mach_task_self_, &mut sample.ports, &mut sample.count) == 0,
            "thread enumeration"
        );
        let mut ids = Vec::new();
        for i in 0..sample.count {
            let port = *sample.ports.add(i as usize);
            let mut info = Extended {
                times: [0; 2],
                scheduling: [0; 8],
                name: [0; 64],
            };
            let mut count = (std::mem::size_of::<Extended>() / 4) as u32;
            // Use retained Mach rights, never a pthread pointer whose lifetime could race exit.
            if thread_info(port, 5, (&mut info as *mut Extended).cast(), &mut count) != 0 {
                continue;
            }
            ensure!(count == 28 && info.name.contains(&0), "thread metadata");
            if CStr::from_ptr(info.name.as_ptr()).to_bytes() == b"continuation-native" {
                let mut info = [0u64; 3];
                let mut count = 6;
                ensure!(
                    thread_info(port, 4, info.as_mut_ptr().cast(), &mut count) == 0 && count == 6,
                    "thread identity"
                );
                ids.push(info[0]);
            }
        }
        ensure!(ids.len() == 1, "worker count");
        Ok((ids[0], sample.count))
    }
}
pub struct Probe(mpsc::Sender<tokio::sync::oneshot::Sender<Result<(i32, Duration)>>>);
impl Probe {
    pub fn new(pid: i32) -> Self {
        let (tx, rx) = mpsc::channel::<tokio::sync::oneshot::Sender<Result<(i32, Duration)>>>();
        std::thread::spawn(move || unsafe {
            use cocoa::base::id;
            use objc::{class, msg_send, sel, sel_impl};
            let pool: id = msg_send![class!(NSAutoreleasePool), new];
            {
                let app = Owned(AXUIElementCreateApplication(pid));
                let key = b"AXFocusedUIElement";
                let key = Owned(CFStringCreateWithBytes(
                    std::ptr::null(),
                    key.as_ptr(),
                    key.len() as isize,
                    0x08000100,
                    false,
                ));
                for reply in rx {
                    let result = (|| -> Result<_> {
                        ensure!(!app.0.is_null() && !key.0.is_null(), "probe allocation");
                        ensure!(
                            CFGetTypeID(app.0) == AXUIElementGetTypeID(),
                            "application type"
                        );
                        let mut actual = 0;
                        ensure!(
                            AXUIElementGetPid(app.0, &mut actual) == 0 && actual == pid,
                            "application pid"
                        );
                        ensure!(
                            AXUIElementSetMessagingTimeout(app.0, 0.1) == 0,
                            "probe budget"
                        );
                        let mut value = std::ptr::null();
                        let start = Instant::now();
                        let error = AXUIElementCopyAttributeValue(app.0, key.0, &mut value);
                        let elapsed = start.elapsed();
                        let value = Owned(value);
                        if error == 0 {
                            ensure!(
                                !value.0.is_null()
                                    && CFGetTypeID(value.0) == AXUIElementGetTypeID(),
                                "focused type"
                            );
                            ensure!(
                                AXUIElementGetPid(value.0, &mut actual) == 0 && actual == pid,
                                "focused pid"
                            );
                        }
                        Ok((error, elapsed))
                    })();
                    let _ = reply.send(result);
                }
            }
            let _: () = msg_send![pool, drain];
        });
        Self(tx)
    }
    pub async fn read(&self) -> Result<(i32, Duration)> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.0
            .send(tx)
            .map_err(|_| anyhow::anyhow!("probe closed"))?;
        tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .map_err(|_| anyhow::anyhow!("probe deadline"))?
            .map_err(|_| anyhow::anyhow!("probe reply"))?
    }
}
