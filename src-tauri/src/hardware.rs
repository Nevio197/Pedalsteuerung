use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
#[cfg(target_os = "macos")]
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessMode {
    Exclusive,
    Shared,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HardwareEvent {
    Connected,
    Disconnected { reason: String },
    Button { button: u8, pressed: bool },
}

pub struct HardwareReader {
    signal: Arc<StopSignal>,
    worker: Option<JoinHandle<()>>,
}

impl HardwareReader {
    /// The callback runs on a dedicated reader thread. It should enqueue the event.
    pub fn spawn(mode: AccessMode, emit: impl Fn(HardwareEvent) + Send + 'static) -> Self {
        let signal = Arc::new(StopSignal::default());
        let thread_signal = Arc::clone(&signal);
        let worker = thread::spawn(move || platform::read(mode, &thread_signal, emit));
        Self {
            signal,
            worker: Some(worker),
        }
    }

    /// Close the device before starting another reader. No new callbacks follow this call.
    pub fn stop(&mut self) {
        self.signal.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for HardwareReader {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct StopSignal {
    stopped: Mutex<bool>,
    changed: Condvar,
}

impl StopSignal {
    fn stop(&self) {
        *self.stopped.lock().unwrap() = true;
        self.changed.notify_all();
    }

    #[cfg(target_os = "macos")]
    fn is_stopped(&self) -> bool {
        *self.stopped.lock().unwrap()
    }

    #[cfg(target_os = "macos")]
    fn wait(&self, duration: Duration) {
        let stopped = self.stopped.lock().unwrap();
        let _ = self
            .changed
            .wait_timeout_while(stopped, duration, |stopped| !*stopped);
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct ButtonState(u8);

#[cfg(any(target_os = "macos", test))]
impl ButtonState {
    fn update(&mut self, buttons: u8) -> impl Iterator<Item = HardwareEvent> + use<> {
        let buttons = buttons & 0b111;
        let changed = self.0 ^ buttons;
        self.0 = buttons;
        (0..3).filter_map(move |bit| {
            let flag = 1 << bit;
            (changed & flag != 0).then_some(HardwareEvent::Button {
                button: bit + 1,
                pressed: buttons & flag != 0,
            })
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{AccessMode, HardwareEvent, StopSignal};

    pub(super) fn read(_mode: AccessMode, _stop: &StopSignal, emit: impl Fn(HardwareEvent)) {
        emit(HardwareEvent::Disconnected {
            reason: "Der direkte USB-Zugriff ist in dieser Version für macOS verfügbar.".into(),
        });
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{AccessMode, ButtonState, Duration, HardwareEvent, StopSignal};
    use std::ffi::{CStr, c_char, c_void};
    use std::ptr::{NonNull, null, null_mut};

    const VENDOR_ID: i32 = 0x0911;
    const PRODUCT_ID: i32 = 0x1844;
    const JOYSTICK_PAGE: i32 = 1;
    const JOYSTICK_USAGE: i32 = 4;
    const SEIZE_DEVICE: u32 = 1;
    const REPORT_SIZE: usize = 3;
    const INPUT_REPORT: u32 = 0;
    const CF_NUMBER_INT_TYPE: isize = 9;
    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const EXCLUSIVE_ACCESS: u32 = 0xe000_02c5;
    const NOT_PRIVILEGED: u32 = 0xe000_02c1;
    const NOT_PERMITTED: u32 = 0xe000_02e2;
    const RETRY_DELAY: Duration = Duration::from_secs(1);
    const RUN_LOOP_INTERVAL: f64 = 0.05;

    type CfRef = *const c_void;
    type HidRef = *mut c_void;
    type RemovalCallback = unsafe extern "C" fn(*mut c_void, i32, *mut c_void);
    type ReportCallback =
        unsafe extern "C" fn(*mut c_void, i32, *mut c_void, u32, u32, *mut u8, isize);

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: CfRef;
        fn CFRetain(reference: CfRef) -> CfRef;
        fn CFRelease(reference: CfRef);
        fn CFGetTypeID(reference: CfRef) -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberGetValue(number: CfRef, kind: isize, result: *mut c_void) -> u8;
        fn CFSetGetCount(set: CfRef) -> isize;
        fn CFSetGetValues(set: CfRef, values: *mut CfRef);
        fn CFStringCreateWithCString(allocator: CfRef, text: *const c_char, encoding: u32)
        -> CfRef;
        fn CFRunLoopGetCurrent() -> CfRef;
        fn CFRunLoopRunInMode(mode: CfRef, seconds: f64, return_after_source: u8) -> i32;
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOHIDManagerCreate(allocator: CfRef, options: u32) -> HidRef;
        fn IOHIDManagerSetDeviceMatching(manager: HidRef, matching: CfRef);
        fn IOHIDManagerCopyDevices(manager: HidRef) -> CfRef;
        fn IOHIDDeviceGetProperty(device: HidRef, key: CfRef) -> CfRef;
        fn IOHIDDeviceOpen(device: HidRef, options: u32) -> i32;
        fn IOHIDDeviceClose(device: HidRef, options: u32) -> i32;
        fn IOHIDDeviceRegisterRemovalCallback(
            device: HidRef,
            callback: Option<RemovalCallback>,
            context: *mut c_void,
        );
        fn IOHIDDeviceRegisterInputReportCallback(
            device: HidRef,
            report: *mut u8,
            length: isize,
            callback: Option<ReportCallback>,
            context: *mut c_void,
        );
        fn IOHIDDeviceScheduleWithRunLoop(device: HidRef, run_loop: CfRef, mode: CfRef);
        fn IOHIDDeviceUnscheduleFromRunLoop(device: HidRef, run_loop: CfRef, mode: CfRef);
    }

    // References returned by Core Foundation's Create/Copy/Retain rules are released once.
    struct OwnedCf(NonNull<c_void>);

    impl OwnedCf {
        fn from_owned(reference: CfRef) -> Option<Self> {
            NonNull::new(reference.cast_mut()).map(Self)
        }

        fn get(&self) -> HidRef {
            self.0.as_ptr()
        }
    }

    impl Drop for OwnedCf {
        fn drop(&mut self) {
            unsafe { CFRelease(self.get()) };
        }
    }

    fn find_pedal() -> Result<OwnedCf, String> {
        let manager = OwnedCf::from_owned(unsafe { IOHIDManagerCreate(null(), 0) })
            .ok_or_else(|| "macOS konnte die USB-Gerätesuche nicht starten.".to_owned())?;
        // Enumerating is read-only; no device is opened via the manager.
        unsafe { IOHIDManagerSetDeviceMatching(manager.get(), null()) };
        let devices = OwnedCf::from_owned(unsafe { IOHIDManagerCopyDevices(manager.get()) })
            .ok_or_else(|| "Philips-Pedal wird gesucht.".to_owned())?;
        let mut entries = vec![null(); unsafe { CFSetGetCount(devices.get()) } as usize];
        unsafe { CFSetGetValues(devices.get(), entries.as_mut_ptr()) };
        let mut matches = entries.into_iter().filter(|entry| {
            let entry = entry.cast_mut();
            number(entry, c"VendorID") == Some(VENDOR_ID)
                && number(entry, c"ProductID") == Some(PRODUCT_ID)
                && number(entry, c"PrimaryUsagePage") == Some(JOYSTICK_PAGE)
                && number(entry, c"PrimaryUsage") == Some(JOYSTICK_USAGE)
        });
        let device = matches
            .next()
            .ok_or_else(|| "Philips-Pedal wird gesucht.".to_owned())?;
        if matches.next().is_some() {
            return Err(
                "Mehrere passende Philips-Pedale sind verbunden. Schließe nur eines an.".into(),
            );
        }
        OwnedCf::from_owned(unsafe { CFRetain(device) })
            .ok_or_else(|| "Das Philips-Pedal ist nicht verfügbar.".to_owned())
    }

    fn number(device: HidRef, key: &CStr) -> Option<i32> {
        let key = OwnedCf::from_owned(unsafe {
            CFStringCreateWithCString(null(), key.as_ptr(), CF_STRING_ENCODING_UTF8)
        })?;
        let value = unsafe { IOHIDDeviceGetProperty(device, key.get()) };
        let mut result = 0_i32;
        unsafe {
            (!value.is_null()
                && CFGetTypeID(value) == CFNumberGetTypeID()
                && CFNumberGetValue(value, CF_NUMBER_INT_TYPE, (&mut result as *mut i32).cast())
                    != 0)
                .then_some(result)
        }
    }

    struct Connection {
        device: OwnedCf,
        flags: u32,
        callback_state: NonNull<CallbackState>,
        report_buffer: NonNull<[u8; REPORT_SIZE]>,
    }

    #[derive(Default)]
    struct CallbackState {
        removed: bool,
        buttons: ButtonState,
        pending: Vec<HardwareEvent>,
    }

    impl Connection {
        fn open(device: OwnedCf, mode: AccessMode) -> Result<Self, String> {
            let flags = match mode {
                AccessMode::Exclusive => SEIZE_DEVICE,
                AccessMode::Shared => 0,
            };
            let result = unsafe { IOHIDDeviceOpen(device.get(), flags) } as u32;
            if result != 0 {
                return Err(match result {
                    EXCLUSIVE_ACCESS => "Das Philips-Pedal ist bereits exklusiv geöffnet.".into(),
                    NOT_PRIVILEGED | NOT_PERMITTED => {
                        "macOS hat den Zugriff auf das Philips-Pedal verweigert.".into()
                    }
                    _ => format!("Philips-Pedal konnte nicht geöffnet werden (0x{result:08x})."),
                });
            }
            let connection = Self {
                device,
                flags,
                callback_state: NonNull::from(Box::leak(Box::default())),
                report_buffer: NonNull::from(Box::leak(Box::new([0; REPORT_SIZE]))),
            };
            let context = connection.callback_state.as_ptr().cast();
            // Device and callbacks stay on this thread. Only the C callback accesses these
            // allocations during the run loop, and Drop reclaims them after unregistering.
            unsafe {
                IOHIDDeviceRegisterRemovalCallback(connection.device.get(), Some(removed), context);
                IOHIDDeviceRegisterInputReportCallback(
                    connection.device.get(),
                    connection.report_buffer.as_ptr().cast(),
                    REPORT_SIZE as isize,
                    Some(report),
                    context,
                );
                IOHIDDeviceScheduleWithRunLoop(
                    connection.device.get(),
                    CFRunLoopGetCurrent(),
                    kCFRunLoopDefaultMode,
                );
            }
            Ok(connection)
        }

        fn read(&mut self, stop: &StopSignal, emit: &impl Fn(HardwareEvent)) {
            while !stop.is_stopped() {
                unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, RUN_LOOP_INTERVAL, 1) };
                // The callbacks run synchronously on this run loop, outside this borrow.
                let state = unsafe { self.callback_state.as_mut() };
                if state.removed || stop.is_stopped() {
                    break;
                }
                for event in state.pending.drain(..) {
                    emit(event);
                }
            }
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            unsafe {
                IOHIDDeviceUnscheduleFromRunLoop(
                    self.device.get(),
                    CFRunLoopGetCurrent(),
                    kCFRunLoopDefaultMode,
                );
                IOHIDDeviceRegisterRemovalCallback(self.device.get(), None, null_mut());
                IOHIDDeviceRegisterInputReportCallback(
                    self.device.get(),
                    self.report_buffer.as_ptr().cast(),
                    REPORT_SIZE as isize,
                    None,
                    null_mut(),
                );
                IOHIDDeviceClose(self.device.get(), self.flags);
                drop(Box::from_raw(self.callback_state.as_ptr()));
                drop(Box::from_raw(self.report_buffer.as_ptr()));
            }
        }
    }

    unsafe extern "C" fn removed(context: *mut c_void, _result: i32, _sender: *mut c_void) {
        unsafe { (*context.cast::<CallbackState>()).removed = true };
    }

    unsafe extern "C" fn report(
        context: *mut c_void,
        result: i32,
        _sender: *mut c_void,
        report_type: u32,
        report_id: u32,
        bytes: *mut u8,
        length: isize,
    ) {
        if result == 0
            && report_type == INPUT_REPORT
            && report_id == 0
            && length == REPORT_SIZE as isize
            && !bytes.is_null()
        {
            let state = unsafe { &mut *context.cast::<CallbackState>() };
            // The connected joystick has three buttons in byte zero. Bytes one and two are axes.
            state
                .pending
                .extend(state.buttons.update(unsafe { *bytes }));
        }
    }

    pub(super) fn read(mode: AccessMode, stop: &StopSignal, emit: impl Fn(HardwareEvent)) {
        let mut last_error = None;
        while !stop.is_stopped() {
            let error = match find_pedal().and_then(|device| Connection::open(device, mode)) {
                Ok(mut connection) => {
                    if stop.is_stopped() {
                        return;
                    }
                    last_error = None;
                    emit(HardwareEvent::Connected);
                    connection.read(stop, &emit);
                    "Philips-Pedal getrennt. Warte auf die Wiederverbindung.".into()
                }
                Err(error) => error,
            };
            if stop.is_stopped() {
                return;
            }
            if last_error.as_ref() != Some(&error) {
                emit(HardwareEvent::Disconnected {
                    reason: error.clone(),
                });
                last_error = Some(error);
            }
            stop.wait(RETRY_DELAY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ButtonState, HardwareEvent::Button};

    #[test]
    fn emits_only_changed_button_bits() {
        let mut buttons = ButtonState::default();
        assert_eq!(
            buttons.update(0b1111_1101).collect::<Vec<_>>(),
            vec![
                Button {
                    button: 1,
                    pressed: true
                },
                Button {
                    button: 3,
                    pressed: true
                }
            ],
        );
        assert_eq!(buttons.update(0b101).count(), 0);
        assert_eq!(
            buttons.update(0b010).collect::<Vec<_>>(),
            vec![
                Button {
                    button: 1,
                    pressed: false
                },
                Button {
                    button: 2,
                    pressed: true
                },
                Button {
                    button: 3,
                    pressed: false
                },
            ],
        );
    }
}
