// Copyright 2021 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

use accesskit::{ActionHandler, NodeId, TreeUpdate};
use lazy_static::lazy_static;
use parking_lot::{const_mutex, Condvar, Mutex};
use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};
use windows as Windows;
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::Gdi::ValidateRect,
        System::{Com::*, LibraryLoader::GetModuleHandleW},
        UI::{Accessibility::*, WindowsAndMessaging::*},
    },
};

use super::Adapter;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

lazy_static! {
    static ref WIN32_INSTANCE: HINSTANCE = {
        let instance = unsafe { GetModuleHandleW(None) };
        if instance.0 == 0 {
            let result: Result<()> = Err(Error::from_win32());
            result.unwrap();
        }
        instance
    };

    static ref DEFAULT_CURSOR: HCURSOR = {
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) };
        if cursor.0 == 0 {
            let result: Result<()> = Err(Error::from_win32());
            result.unwrap();
        }
        cursor
    };

    static ref WINDOW_CLASS_ATOM: u16 = {
        // The following is a combination of the implementation of
        // IntoParam<PWSTR> and the class registration function from winit.
        let class_name_wsz: Vec<_> = "AccessKitTest"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let wc = WNDCLASSW {
            hCursor: *DEFAULT_CURSOR,
            hInstance: *WIN32_INSTANCE,
            lpszClassName: PWSTR(class_name_wsz.as_ptr() as _),
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            ..Default::default()
        };

        let atom = unsafe { RegisterClassW(&wc) };
        if atom == 0 {
            let result: Result<()> = Err(Error::from_win32());
            result.unwrap();
        }
        atom
    };
}

struct InnerWindowState {
    focus: NodeId,
    is_window_focused: bool,
}

struct WindowState {
    adapter: Adapter,
    inner_state: Rc<RefCell<InnerWindowState>>,
}

unsafe fn get_window_state(window: HWND) -> *const WindowState {
    GetWindowLongPtrW(window, GWLP_USERDATA) as _
}

fn update_focus(window: HWND, is_window_focused: bool) {
    let window_state = unsafe { &*get_window_state(window) };
    let mut inner_state = window_state.inner_state.borrow_mut();
    inner_state.is_window_focused = is_window_focused;
    let focus = inner_state.focus;
    drop(inner_state);
    let events = window_state.adapter.update_if_active(|| TreeUpdate {
        clear: None,
        nodes: vec![],
        tree: None,
        focus: is_window_focused.then(|| focus),
    });
    events.raise();
}

struct WindowCreateParams(TreeUpdate, NodeId, Box<dyn ActionHandler>);

extern "system" fn wndproc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message as u32 {
        WM_NCCREATE => {
            let create_struct: &CREATESTRUCTW = unsafe { &mut *(lparam.0 as *mut _) };
            let create_params: Box<WindowCreateParams> =
                unsafe { Box::from_raw(create_struct.lpCreateParams as _) };
            let WindowCreateParams(initial_state, initial_focus, action_handler) = *create_params;
            let inner_state = Rc::new(RefCell::new(InnerWindowState {
                focus: initial_focus,
                is_window_focused: false,
            }));
            let inner_state_for_tree_init = inner_state.clone();
            let state = Box::new(WindowState {
                adapter: Adapter::new(
                    window,
                    Box::new(move || {
                        let mut result = initial_state;
                        let state = inner_state_for_tree_init.borrow();
                        result.focus = state.is_window_focused.then(|| state.focus);
                        result
                    }),
                    action_handler,
                ),
                inner_state,
            });
            unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as _) };
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_PAINT => {
            unsafe { ValidateRect(window, std::ptr::null()) }.unwrap();
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr = unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, 0) };
            if ptr != 0 {
                let _dropped: Box<WindowState> = unsafe { Box::from_raw(ptr as _) };
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_GETOBJECT => {
            let window_state = unsafe { get_window_state(window) };
            if window_state.is_null() {
                // We need to be prepared to gracefully handle WM_GETOBJECT
                // while the window is being destroyed; this can happen if
                // the thread is using a COM STA.
                return unsafe { DefWindowProcW(window, message, wparam, lparam) };
            }
            let window_state = unsafe { &*window_state };
            let result = window_state.adapter.handle_wm_getobject(wparam, lparam);
            result.map_or_else(
                || unsafe { DefWindowProcW(window, message, wparam, lparam) },
                |result| result.into(),
            )
        }
        WM_SETFOCUS | WM_EXITMENULOOP | WM_EXITSIZEMOVE => {
            update_focus(window, true);
            LRESULT(0)
        }
        WM_KILLFOCUS | WM_ENTERMENULOOP | WM_ENTERSIZEMOVE => {
            update_focus(window, false);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn create_window(
    title: &str,
    initial_state: TreeUpdate,
    initial_focus: NodeId,
    action_handler: Box<dyn ActionHandler>,
) -> Result<HWND> {
    let create_params = Box::new(WindowCreateParams(
        initial_state,
        initial_focus,
        action_handler,
    ));

    let window = unsafe {
        CreateWindowExW(
            Default::default(),
            PWSTR(*WINDOW_CLASS_ATOM as usize as _),
            title,
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            *WIN32_INSTANCE,
            Box::into_raw(create_params) as _,
        )
    };
    if window.0 == 0 {
        return Err(Error::from_win32());
    }

    Ok(window)
}

pub(crate) struct Scope {
    pub(crate) uia: IUIAutomation,
    pub(crate) window: HWND,
}

impl Scope {
    pub(crate) fn show_and_focus_window(&self) {
        unsafe { ShowWindow(self.window, SW_SHOW) };
        unsafe { SetForegroundWindow(self.window) };
    }
}

// It's not safe to run these UI-related tests concurrently.
static MUTEX: Mutex<()> = const_mutex(());

pub(crate) fn scope<F>(
    window_title: &str,
    initial_state: TreeUpdate,
    initial_focus: NodeId,
    action_handler: Box<dyn ActionHandler>,
    f: F,
) -> Result<()>
where
    F: FnOnce(&Scope) -> Result<()>,
{
    let _lock_guard = MUTEX.lock();

    let window_mutex: Mutex<Option<HWND>> = Mutex::new(None);
    let window_cv = Condvar::new();

    crossbeam_utils::thread::scope(|thread_scope| {
        thread_scope.spawn(|_| {
            // We explicitly don't want to initialize COM on the provider thread,
            // because we want to make sure that the provider side of UIA works
            // even if COM is never initialized on the provider thread
            // (as is the case if the window is never shown), or if COM is
            // initialized after the window is shown (as is the case,
            // at least on some Windows 10 machines, due to IME support).

            let window =
                create_window(window_title, initial_state, initial_focus, action_handler).unwrap();

            {
                let mut state = window_mutex.lock();
                *state = Some(window);
                window_cv.notify_one();
            }

            let mut message = MSG::default();
            while unsafe { GetMessageW(&mut message, HWND(0), 0, 0) }.into() {
                unsafe { TranslateMessage(&message) };
                unsafe { DispatchMessageW(&message) };
            }
        });

        let window = {
            let mut state = window_mutex.lock();
            if state.is_none() {
                window_cv.wait(&mut state);
            }
            state.take().unwrap()
        };

        let _window_guard = scopeguard::guard((), |_| {
            unsafe { PostMessageW(window, WM_CLOSE, WPARAM(0), LPARAM(0)) }.unwrap()
        });

        // We must initialize COM before creating the UIA client. The MTA option
        // is cleaner by far, especially when we want to wait for a UIA event
        // handler to be called, and there's no reason not to use it here.
        // Note that we don't initialize COM this way on the provider thread,
        // as explained above. It's also important that we let the provider
        // thread do its forced initialization of UIA, in an environment
        // where COM has not been initialized, before we create the UIA client,
        // which also triggers UIA initialization, in a thread where COM
        // _has_ been initialized. This way, we ensure that the provider side
        // of UIA works even if it is set up in an environment where COM
        // has not been initialized, and that this sequence of events
        // doesn't prevent the UIA client from working.
        unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_MULTITHREADED) }.unwrap();
        let _com_guard = scopeguard::guard((), |_| unsafe { CoUninitialize() });

        let uia: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }?;

        let s = Scope { uia, window };
        f(&s)
    })
    .unwrap()
}

pub(crate) struct ReceivedFocusEvent {
    mutex: Mutex<Option<IUIAutomationElement>>,
    cv: Condvar,
}

impl ReceivedFocusEvent {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            mutex: Mutex::new(None),
            cv: Condvar::new(),
        })
    }

    pub(crate) fn wait<F>(&self, f: F) -> IUIAutomationElement
    where
        F: Fn(&IUIAutomationElement) -> bool,
    {
        let mut received = self.mutex.lock();
        loop {
            if let Some(element) = received.take() {
                if f(&element) {
                    return element;
                }
            }
            let result = self.cv.wait_for(&mut received, DEFAULT_TIMEOUT);
            assert!(!result.timed_out());
        }
    }

    fn put(&self, element: IUIAutomationElement) {
        let mut received = self.mutex.lock();
        *received = Some(element);
        self.cv.notify_one();
    }
}

#[implement(Windows::Win32::UI::Accessibility::IUIAutomationFocusChangedEventHandler)]
pub(crate) struct FocusEventHandler {
    received: Arc<ReceivedFocusEvent>,
}

#[allow(non_snake_case)]
impl FocusEventHandler {
    #[allow(clippy::new_ret_no_self)] // it does return self, but wrapped
    pub(crate) fn new() -> (
        IUIAutomationFocusChangedEventHandler,
        Arc<ReceivedFocusEvent>,
    ) {
        let received = ReceivedFocusEvent::new();
        (
            Self {
                received: received.clone(),
            }
            .into(),
            received,
        )
    }

    fn HandleFocusChangedEvent(&self, sender: &Option<IUIAutomationElement>) -> Result<()> {
        self.received.put(sender.as_ref().unwrap().clone());
        Ok(())
    }
}

mod simple;
