use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(v: bool) {
    VERBOSE.store(v, Ordering::Relaxed);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn debug_eprintln(args: std::fmt::Arguments<'_>) {
    if is_verbose() {
        eprintln!("{}", args);
    }
}

#[macro_export]
macro_rules! vdebug {
    ($($arg:tt)*) => {{
        $crate::logging::debug_eprintln(format_args!($($arg)*));
    }};
}
