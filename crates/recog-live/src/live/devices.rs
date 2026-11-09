//! The audio devices `live` names: `--list-devices`, the default microphone of
//! `--mic-device auto`, BlackHole for `--system-audio device:auto`, and a device index
//! turned into the name the microphone worker opens.
//!
//! Python asks PortAudio (sounddevice); here CoreAudio's list comes through cpal, in
//! the order CoreAudio gives it, which is the order PortAudio numbers the devices on a
//! Mac. Nothing is opened: listing devices needs no permission. On Linux the
//! microphone and the system audio come from the sound server (`parec`), so the list
//! is `pactl list short sources`.

use std::process::Command;

/// One device as the lookups need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub index: usize,
    pub name: String,
    pub inputs: u16,
    pub outputs: u16,
    pub default_input: bool,
    pub default_output: bool,
}

/// The audio devices of this machine (CoreAudio on a Mac; none elsewhere).
#[must_use]
pub fn list_devices() -> Vec<DeviceInfo> {
    #[cfg(target_os = "macos")]
    {
        coreaudio_devices()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
fn coreaudio_devices() -> Vec<DeviceInfo> {
    use cpal::traits::{DeviceTrait as _, HostTrait as _};

    let host = cpal::default_host();
    let default_in = host.default_input_device().and_then(|d| d.id().ok());
    let default_out = host.default_output_device().and_then(|d| d.id().ok());
    let channels = |configs: Option<Vec<u16>>| configs.and_then(|c| c.into_iter().max()).unwrap_or(0);
    host.devices()
        .map(|devices| {
            devices
                .enumerate()
                .map(|(index, device)| {
                    let id = device.id().ok();
                    let inputs =
                        channels(device.supported_input_configs().ok().map(|c| c.map(|r| r.channels()).collect()));
                    let outputs =
                        channels(device.supported_output_configs().ok().map(|c| c.map(|r| r.channels()).collect()));
                    DeviceInfo {
                        index,
                        name: device.description().map(|d| d.name().to_owned()).unwrap_or_default(),
                        inputs,
                        outputs,
                        default_input: is_default(id.as_ref(), default_in.as_ref()),
                        default_output: is_default(id.as_ref(), default_out.as_ref()),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a device is the default one: both ids known and equal.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_default<T: PartialEq>(id: Option<&T>, default: Option<&T>) -> bool {
    id.is_some() && id == default
}

/// `find_default_mic`: on a Mac the built-in MacBook microphone (or a built-in or
/// internal input), then a device proxying the sound server (`pulse`, `pipewire`,
/// `default`), then the default input.
#[must_use]
pub fn find_default_mic(devices: &[DeviceInfo], platform: &str) -> Option<usize> {
    let inputs = || devices.iter().filter(|d| d.inputs > 0);
    if platform == "darwin" {
        if let Some(d) = inputs().find(|d| {
            let name = d.name.to_lowercase();
            name.contains("macbook") && (name.contains("microphone") || name.contains("микрофон"))
        }) {
            return Some(d.index);
        }
        if let Some(d) = inputs().find(|d| {
            let name = d.name.to_lowercase();
            name.contains("built-in") || name.contains("internal")
        }) {
            return Some(d.index);
        }
    }
    for preferred in ["pulse", "pipewire", "default"] {
        if let Some(d) = inputs().find(|d| recog_core::py::strip(&d.name).to_lowercase() == preferred) {
            return Some(d.index);
        }
    }
    devices.iter().find(|d| d.default_input).map(|d| d.index)
}

/// `find_blackhole`: the first input whose name holds "blackhole".
#[must_use]
pub fn find_blackhole(devices: &[DeviceInfo]) -> Option<usize> {
    devices.iter().find(|d| d.inputs > 0 && d.name.to_lowercase().contains("blackhole")).map(|d| d.index)
}

/// The device of `--mic-device <id>` or `device:<id>`: an index, or a name taken as is.
#[must_use]
pub fn device_name(devices: &[DeviceInfo], id: &str) -> Option<String> {
    match id.trim().parse::<usize>() {
        Ok(index) => devices.iter().find(|d| d.index == index).map(|d| d.name.clone()),
        Err(_) => Some(id.to_owned()),
    }
}

/// sounddevice's `DeviceList` text: one line per device, `>` the default input, `<`
/// the default output, `*` both.
#[must_use]
pub fn format_devices(devices: &[DeviceInfo]) -> String {
    let width = devices.len().saturating_sub(1).to_string().len();
    devices
        .iter()
        .map(|d| {
            let mark = match (d.default_input, d.default_output) {
                (true, true) => '*',
                (true, false) => '>',
                (false, true) => '<',
                (false, false) => ' ',
            };
            format!("{mark} {:>width$} {}, Core Audio ({} in, {} out)", d.index, d.name, d.inputs, d.outputs)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `print_pulse_sources`: the PulseAudio/PipeWire sources, which PortAudio does not
/// show; `(out, err)` are the stdout and stderr lines.
#[must_use]
pub fn pulse_sources(run: &dyn Fn(&[&str]) -> std::io::Result<std::process::Output>) -> (Vec<String>, Vec<String>) {
    match run(&["pactl", "list", "short", "sources"]) {
        Err(e) => (Vec::new(), vec![format!("\n(pactl недоступен: {e})")]),
        Ok(output) if !output.status.success() => (
            Vec::new(),
            vec![format!(
                "\n(pactl list short sources завершился с кодом {})",
                output.status.code().map_or_else(|| "None".to_owned(), |c| c.to_string())
            )],
        ),
        Ok(output) => (
            vec![
                String::new(),
                "Источники PulseAudio/PipeWire (pactl list short sources):".into(),
                String::from_utf8_lossy(&output.stdout).trim_end().to_owned(),
            ],
            Vec::new(),
        ),
    }
}

/// Runs a command with a two-second limit, as `subprocess.run(timeout=PACTL_TIMEOUT_S)`.
///
/// # Errors
/// The command could not start or did not finish in time.
pub fn run_with_timeout(argv: &[&str]) -> std::io::Result<std::process::Output> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("Command '{argv:?}' timed out after 2.0 seconds"),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(index: usize, name: &str, inputs: u16) -> DeviceInfo {
        DeviceInfo { index, name: name.into(), inputs, outputs: 0, default_input: false, default_output: false }
    }

    #[test]
    fn a_mac_with_a_usb_mic_first_find_default_mic_picks_the_macbook_microphone() {
        let devices =
            [device(0, "USB Mic", 1), device(1, "MacBook Pro Speakers", 0), device(2, "MacBook Pro Microphone", 1)];
        assert_eq!(find_default_mic(&devices, "darwin"), Some(2));
    }

    #[test]
    fn a_mac_without_a_macbook_mic_find_default_mic_takes_a_built_in_input() {
        let devices = [device(0, "USB Mic", 1), device(1, "Built-in Input", 2)];
        assert_eq!(find_default_mic(&devices, "darwin"), Some(1));
    }

    #[test]
    fn a_linux_box_find_default_mic_prefers_the_sound_server_device() {
        let devices = [device(0, "HDA Intel", 2), device(1, "default", 2), device(2, " pulse ", 2)];
        assert_eq!(find_default_mic(&devices, "linux"), Some(2));
    }

    #[test]
    fn no_match_find_default_mic_falls_back_to_the_default_input_then_none() {
        let mut devices = [device(0, "Some Mic", 1), device(1, "Other", 1)];
        assert_eq!(find_default_mic(&devices, "linux"), None);
        devices[1].default_input = true;
        assert_eq!(find_default_mic(&devices, "linux"), Some(1));
    }

    #[test]
    fn a_blackhole_input_find_blackhole_finds_it_and_skips_outputs() {
        let devices = [device(0, "BlackHole 2ch", 0), device(1, "BlackHole 16ch", 16)];
        assert_eq!(find_blackhole(&devices), Some(1));
        assert_eq!(find_blackhole(&devices[..1]), None);
    }

    #[test]
    fn an_index_or_a_name_device_name_resolves_it() {
        let devices = [device(0, "A", 1), device(1, "B", 1)];
        assert_eq!(device_name(&devices, "1").as_deref(), Some("B"));
        assert_eq!(device_name(&devices, "7"), None);
        assert_eq!(device_name(&devices, "My Mic").as_deref(), Some("My Mic"));
    }

    #[test]
    fn defaults_marked_format_devices_writes_sounddevice_lines() {
        let mut devices: Vec<DeviceInfo> = (0..11).map(|i| device(i, &format!("D{i}"), 1)).collect();
        devices[0].default_input = true;
        devices[1].default_output = true;
        let text = format_devices(&devices);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], ">  0 D0, Core Audio (1 in, 0 out)");
        assert_eq!(lines[1], "<  1 D1, Core Audio (1 in, 0 out)");
        assert_eq!(lines[10], "  10 D10, Core Audio (1 in, 0 out)");
    }

    #[test]
    fn pactl_missing_or_failing_pulse_sources_says_so_on_stderr() {
        let (out, err) = pulse_sources(&|_| Err(std::io::Error::from(std::io::ErrorKind::NotFound)));
        assert!(out.is_empty());
        assert!(err[0].starts_with("\n(pactl недоступен: "));
        let (out, err) = pulse_sources(&|_| Command::new("sh").args(["-c", "exit 3"]).output());
        assert!(out.is_empty());
        assert_eq!(err, vec!["\n(pactl list short sources завершился с кодом 3)"]);
        let (out, _) = pulse_sources(&|_| Command::new("sh").args(["-c", "printf '0\\tmon\\n'"]).output());
        assert_eq!(out, vec!["", "Источники PulseAudio/PipeWire (pactl list short sources):", "0\tmon"]);
    }

    #[test]
    fn device_ids_known_or_not_when_compared_with_the_default_then_only_a_known_equal_id_is_it() {
        // given / when / then
        assert!(is_default(Some(&7), Some(&7)));
        assert!(!is_default(Some(&7), Some(&8)));
        assert!(!is_default(Some(&7), None));
        assert!(!is_default::<i32>(None, None));
    }
}
