/// Developer diagnostics are intentionally absent from production GUI stderr.
/// Calls remain type-checked in release builds without evaluating formatting
/// arguments or writing arbitrary source errors, paths, identities, or content.
#[macro_export]
macro_rules! murmur_dev_log {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        {
            std::eprintln!($($arg)*);
        }
        #[cfg(not(debug_assertions))]
        {
            if false {
                std::eprintln!($($arg)*);
            }
        }
    }};
}
