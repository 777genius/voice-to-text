//! Desktop power lifecycle adapter.
//!
//! Tauri does not expose desktop suspend events. On macOS the supported source
//! is `NSWorkspace.notificationCenter`, so bridge only the two notification
//! callbacks into the recording input/coordinator boundary.

#[cfg(target_os = "macos")]
mod macos {
    use cocoa::base::{id, nil};
    use cocoa::foundation::NSString;
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::OnceLock;
    use tauri::AppHandle;

    const HANDLE_IVAR: &str = "rustAppHandle";
    const OBSERVER_CLASS: &str = "VoicetextRecordingPowerObserver";

    extern "C" fn workspace_will_sleep(this: &Object, _selector: Sel, _notification: id) {
        let app_handle = unsafe { app_handle(this) };
        log::info!("macOS workspace will sleep; forcing recording Off");
        super::super::commands::force_off_recording_for_system_sleep(app_handle.clone());
    }

    extern "C" fn workspace_did_wake(this: &Object, _selector: Sel, _notification: id) {
        let app_handle = unsafe { app_handle(this) };
        log::info!("macOS workspace did wake; resetting recording input latches");
        super::super::commands::reset_recording_gestures_after_system_wake(app_handle);
    }

    unsafe fn app_handle(observer: &Object) -> &'static AppHandle {
        let pointer = *observer.get_ivar::<usize>(HANDLE_IVAR);
        &*(pointer as *const AppHandle)
    }

    fn observer_class() -> &'static Class {
        static CLASS: OnceLock<&'static Class> = OnceLock::new();
        CLASS.get_or_init(|| {
            if let Some(existing) = Class::get(OBSERVER_CLASS) {
                return existing;
            }
            let mut declaration = ClassDecl::new(OBSERVER_CLASS, class!(NSObject))
                .expect("power observer Objective-C class declaration");
            declaration.add_ivar::<usize>(HANDLE_IVAR);
            unsafe {
                declaration.add_method(
                    sel!(workspaceWillSleep:),
                    workspace_will_sleep as extern "C" fn(&Object, Sel, id),
                );
                declaration.add_method(
                    sel!(workspaceDidWake:),
                    workspace_did_wake as extern "C" fn(&Object, Sel, id),
                );
            }
            declaration.register()
        })
    }

    pub(super) fn register(app_handle: AppHandle) -> Result<(), String> {
        unsafe {
            let observer: id = msg_send![observer_class(), new];
            if observer == nil {
                return Err("failed to allocate macOS power observer".to_string());
            }

            // Both allocations intentionally live for the process lifetime. The
            // notification center may invoke the selector until application exit.
            let app_pointer = Box::into_raw(Box::new(app_handle)) as usize;
            (&mut *observer).set_ivar(HANDLE_IVAR, app_pointer);

            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let center: id = msg_send![workspace, notificationCenter];
            let will_sleep = NSString::alloc(nil).init_str("NSWorkspaceWillSleepNotification");
            let did_wake = NSString::alloc(nil).init_str("NSWorkspaceDidWakeNotification");

            let _: () = msg_send![center,
                addObserver: observer
                selector: sel!(workspaceWillSleep:)
                name: will_sleep
                object: nil
            ];
            let _: () = msg_send![center,
                addObserver: observer
                selector: sel!(workspaceDidWake:)
                name: did_wake
                object: nil
            ];
            let _: () = msg_send![will_sleep, release];
            let _: () = msg_send![did_wake, release];
        }
        Ok(())
    }
}

pub fn register_recording_power_observer(app_handle: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return macos::register(app_handle);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app_handle;
        Ok(())
    }
}
