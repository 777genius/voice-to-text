use super::warm_dictation_input::{
    WarmDictationInput, WarmInputFormat, WarmNativeFactory, WarmNativeInput, WarmRawBlock,
    WarmRawCallback,
};
use super::SystemAudioCapture;
use crate::domain::{AudioCaptureErrorCallback, AudioError, AudioResult};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, SupportedStreamConfig};
use std::sync::Arc;

struct CpalFactory {
    requested: Option<String>,
}
struct CpalInput {
    stream: Stream,
    device: Device,
    config: SupportedStreamConfig,
    requested: Option<String>,
    format: WarmInputFormat,
    default_device_id: Option<u32>,
}

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}
#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const std::ffi::c_void,
        size: *mut u32,
        data: *mut std::ffi::c_void,
    ) -> i32;
}

fn property_u32(object: u32, selector: &[u8; 4]) -> AudioResult<u32> {
    let address = PropertyAddress {
        selector: u32::from_be_bytes(*selector),
        scope: u32::from_be_bytes(*b"glob"),
        element: 0,
    };
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // CoreAudio writes exactly one UInt32 into this stack-owned value.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut u32).cast(),
        )
    };
    if status != 0 || size != std::mem::size_of::<u32>() as u32 {
        return Err(AudioError::Capture(format!(
            "CoreAudio input property failed: {status}"
        )));
    }
    Ok(value)
}

fn default_input_id() -> AudioResult<u32> {
    property_u32(1, b"dIn ")
}

fn eligible_transport(transport: u32) -> bool {
    transport == u32::from_be_bytes(*b"bltn")
}

// Compare the actual CoreAudio device identity, not its user-facing name. CPAL
// 0.15.3 exposes this Eq implementation through DeviceInner on macOS.
fn same_device(left: &Device, right: &Device) -> bool {
    match (left.as_inner(), right.as_inner()) {
        (
            cpal::platform::DeviceInner::CoreAudio(left),
            cpal::platform::DeviceInner::CoreAudio(right),
        ) => left == right,
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

fn same_config(left: &SupportedStreamConfig, right: &SupportedStreamConfig) -> bool {
    left.sample_rate() == right.sample_rate()
        && left.channels() == right.channels()
        && left.sample_format() == right.sample_format()
}

fn same_default_route(left: &Device, right: &Device) -> bool {
    same_device(left, right)
        || (left.name().ok() == right.name().ok()
            && left
                .default_input_config()
                .ok()
                .zip(right.default_input_config().ok())
                .is_some_and(|(left, right)| same_config(&left, &right)))
}

impl WarmDictationInput {
    /// Runtime candidate check only. This does not replace measured hardware
    /// acceptance; external transports remain on cold capture. An explicit
    /// name is accepted only when it still resolves to the current default
    /// built-in input, so selecting the same physical route by name does not
    /// accidentally disable the qualified warm path.
    pub fn cpal_is_eligible(requested: Option<&str>) -> bool {
        let transport_is_eligible = default_input_id()
            .and_then(|id| property_u32(id, b"tran"))
            .map(eligible_transport)
            .unwrap_or(false);
        if !transport_is_eligible {
            return false;
        }
        let Some(requested) = requested else {
            return true;
        };
        cpal::default_host()
            .default_input_device()
            .and_then(|device| device.name().ok())
            .is_some_and(|name| SystemAudioCapture::device_name_matches(requested, &name))
    }
    /// Construction creates only the owner thread. Permission and opt-in policy
    /// must be checked by composition before calling prewarm.
    pub fn new_cpal(requested: Option<String>) -> AudioResult<Arc<Self>> {
        if !Self::cpal_is_eligible(requested.as_deref()) {
            return Err(AudioError::Configuration(
                "Warm input is unqualified for this route; use cold capture".into(),
            ));
        }
        Self::with_factory(Box::new(CpalFactory { requested }))
    }
}

impl WarmNativeFactory for CpalFactory {
    fn open(
        &mut self,
        raw: WarmRawCallback,
        error: AudioCaptureErrorCallback,
    ) -> AudioResult<Box<dyn WarmNativeInput>> {
        let host = cpal::default_host();
        let default_before = default_input_id()?;
        if !eligible_transport(property_u32(default_before, b"tran")?) {
            return Err(AudioError::Configuration(
                "Warm input route became unqualified; use cold capture".into(),
            ));
        }
        let (device, config) =
            SystemAudioCapture::select_device_and_config(&host, self.requested.as_deref())
                .or_else(|error| {
                    if self.requested.is_some() {
                        SystemAudioCapture::select_device_and_config(&host, None)
                    } else {
                        Err(error)
                    }
                })?;
        let format = WarmInputFormat {
            sample_rate: config.sample_rate().0,
            channels: config.channels(),
            effective_name: device
                .name()
                .map_err(|e| AudioError::Capture(e.to_string()))?,
        };
        let default_device_id = host
            .default_input_device()
            .filter(|default| same_default_route(default, &device))
            .map(|_| default_before);
        if self.requested.is_some() && default_device_id.is_none() {
            return Err(AudioError::Configuration(
                "Named warm microphone no longer resolves to the default built-in input".into(),
            ));
        }
        if default_device_id.is_some() && default_input_id()? != default_before {
            return Err(AudioError::Capture(
                "Default microphone changed while opening".into(),
            ));
        }
        let stream_config = config.clone().into();
        let error_callback = move |err: cpal::StreamError| {
            error(AudioError::Capture(format!(
                "Warm native stream failed: {err}"
            )))
        };
        let stream = match config.sample_format() {
            SampleFormat::I16 => device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| raw(WarmRawBlock::I16(data)),
                error_callback,
                None,
            ),
            SampleFormat::F32 => device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| raw(WarmRawBlock::F32(data)),
                error_callback,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| raw(WarmRawBlock::U16(data)),
                error_callback,
                None,
            ),
            format => {
                return Err(AudioError::Configuration(format!(
                    "Unsupported warm native format: {format:?}"
                )))
            }
        }
        .map_err(|e| AudioError::Capture(format!("Cannot build warm native input: {e}")))?;
        Ok(Box::new(CpalInput {
            stream,
            device,
            config,
            requested: self.requested.clone(),
            format,
            default_device_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_eligibility_excludes_external_routes() {
        assert!(eligible_transport(u32::from_be_bytes(*b"bltn")));
        for transport in [*b"usb ", *b"blue", *b"blea", *b"virt", *b"????"] {
            assert!(!eligible_transport(u32::from_be_bytes(transport)));
        }
    }
}

impl WarmNativeInput for CpalInput {
    fn format(&self) -> WarmInputFormat {
        self.format.clone()
    }
    fn play(&self) -> AudioResult<()> {
        self.stream
            .play()
            .map_err(|e| AudioError::Capture(format!("Cannot play warm native input: {e}")))
    }
    fn validate_route(&self) -> AudioResult<(bool, bool)> {
        // The qualified production path is the CoreAudio default built-in input.
        // Query its stable object id directly: enumerating every CPAL input here
        // adds roughly 150-200 ms to every otherwise-warm lease attachment.
        if let Some(id) = self.default_device_id {
            let valid = property_u32(id, b"livn")
                .map(|alive| alive != 0)
                .unwrap_or(false);
            if !valid {
                return Ok((false, false));
            }
            if default_input_id()? != id {
                return Ok((true, false));
            }
            let config_valid = self
                .device
                .default_input_config()
                .map(|config| same_config(&config, &self.config))
                .unwrap_or(false);
            return Ok((true, config_valid));
        }

        // Defensive fallback for a route that changed during initial selection.
        // Such a path is not warm-qualified, but still must close deterministically.
        let host = cpal::default_host();
        let inputs: Vec<_> = host
            .input_devices()
            .map_err(|e| AudioError::Capture(e.to_string()))?
            .collect();
        let valid = inputs
            .iter()
            .any(|device| same_device(device, &self.device));
        if !valid {
            return Ok((false, false));
        }
        let config_valid = self
            .device
            .default_input_config()
            .map(|config| same_config(&config, &self.config))
            .unwrap_or(false);
        if !config_valid {
            return Ok((false, false));
        }
        // Validation is frequent and must not emit the cold-open inventory logs.
        let selected = self
            .requested
            .as_ref()
            .and_then(|requested| {
                inputs
                    .iter()
                    .find(|device| {
                        device.name().ok().is_some_and(|name| {
                            SystemAudioCapture::device_name_matches(requested, &name)
                        })
                    })
                    .cloned()
            })
            .or_else(|| host.default_input_device())
            .ok_or_else(|| AudioError::DeviceNotFound("No default input device".into()))?;
        let selected_config = selected
            .default_input_config()
            .map_err(|e| AudioError::Configuration(e.to_string()))?;
        Ok((
            true,
            same_device(&selected, &self.device) && same_config(&selected_config, &self.config),
        ))
    }
}
