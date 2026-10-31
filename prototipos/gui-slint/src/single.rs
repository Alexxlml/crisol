//! One instance per user, and waking the first one up from the second.
//!
//! Slint has nothing for this: it is a named mutex (who is first) and a named event (the second
//! one rings it). Tauri has an official plugin for the same job.

use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject, INFINITE,
};

pub enum Instance {
    /// This is the first one; call [`Waiter::on_second`] to hear about the others.
    First(Waiter),
    /// Another one runs: it was told to show its window.
    Second,
}

pub struct Waiter {
    event: isize,
    // Kept so that the mutex lives as long as the process.
    _mutex: isize,
}

pub fn acquire() -> windows::core::Result<Instance> {
    // `Local\` names belong to this session of this user.
    // SAFETY: plain Win32 calls with valid arguments; the handles stay open on purpose.
    unsafe {
        let mutex = CreateMutexW(None, true, w!("Local\\CrisolGuiPrototype.Mutex"))?;
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        let event = CreateEventW(None, false, false, w!("Local\\CrisolGuiPrototype.Show"))?;
        if exists {
            SetEvent(event)?;
            return Ok(Instance::Second);
        }
        Ok(Instance::First(Waiter {
            event: event.0 as isize,
            _mutex: mutex.0 as isize,
        }))
    }
}

impl Waiter {
    /// Calls `show` from a thread of its own each time a second instance starts.
    pub fn on_second(&self, show: impl Fn() + Send + 'static) {
        let event = self.event;
        std::thread::spawn(move || loop {
            // SAFETY: the handle is never closed.
            let waited = unsafe { WaitForSingleObject(HANDLE(event as *mut _), INFINITE) };
            if waited.0 != 0 {
                return;
            }
            show();
        });
    }
}
