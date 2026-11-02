use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{CLSID_AccPropServices, IAccPropServices, PROPID_ACC_NAME};
use windows::Win32::UI::WindowsAndMessaging::OBJID_CLIENT;
use windows::core::PCWSTR;

use crate::NativeError;

pub(crate) struct Apartment {
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Apartment {
    pub fn new() -> Result<Self, NativeError> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|error| {
                NativeError::Api("Initialize native UI accessibility apartment", error)
            })?;
        Ok(Self {
            _thread: std::marker::PhantomData,
        })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub(crate) struct Labels {
    services: IAccPropServices,
    controls: Vec<HWND>,
}

impl Labels {
    pub fn new(controls: &[(HWND, &str)]) -> Result<Self, NativeError> {
        let services: IAccPropServices =
            unsafe { CoCreateInstance(&CLSID_AccPropServices, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| {
                    NativeError::Api("Create native accessible-name annotations", error)
                })?;
        let mut labels = Self {
            services,
            controls: Vec::new(),
        };
        for &(window, name) in controls {
            let encoded: Vec<_> = name.encode_utf16().chain([0]).collect();
            unsafe {
                labels.services.SetHwndPropStr(
                    window,
                    OBJID_CLIENT.0 as u32,
                    0,
                    PROPID_ACC_NAME,
                    PCWSTR(encoded.as_ptr()),
                )
            }
            .map_err(|error| NativeError::Api("Set original native accessible name", error))?;
            labels.controls.push(window);
        }
        Ok(labels)
    }
}

impl Drop for Labels {
    fn drop(&mut self) {
        for &window in &self.controls {
            if let Err(error) = unsafe {
                self.services
                    .ClearHwndProps(window, OBJID_CLIENT.0 as u32, 0, &[PROPID_ACC_NAME])
            } {
                eprintln!("Native accessible-name cleanup failed: {error}");
            }
        }
    }
}
