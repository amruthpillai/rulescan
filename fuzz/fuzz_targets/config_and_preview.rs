#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|input: &[u8]| {
    if input.len() > 1024 * 1024 {
        return;
    }
    if rulescan::config::reject_yaml_aliases(input).is_ok() {
        if let Ok(config) = serde_yaml_ng::from_slice::<rulescan::config::Config>(input) {
            let _ = config.validate();
        }
    }
    let text =
        String::from_utf8_lossy(&input[..input.len().min(rulescan::model::MAX_PREVIEW_BYTES)]);
    let escaped = rulescan::model::escape(&text, false);
    assert!(!escaped.chars().any(char::is_control));
});
