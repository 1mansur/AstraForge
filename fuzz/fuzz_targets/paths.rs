#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|path: &str| {
    if let Ok(normalized) = astraforge_core::workspace::normalize_path(path) {
        assert!(!normalized.split('/').any(|component| component == ".." || component.eq_ignore_ascii_case(".git")));
        assert!(!std::path::Path::new(&normalized).is_absolute());
        assert_eq!(astraforge_core::workspace::normalize_path(&normalized).unwrap(), normalized);
    }
});
