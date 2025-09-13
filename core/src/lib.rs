pub mod bus;
pub mod cpu_m68k;
pub mod debuggable;
pub mod emulator;
pub mod keymap;
pub mod mac;
pub mod renderer;
pub mod tickable;
pub mod types;
pub mod util;

#[cfg(test)]
pub mod test;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub fn build_version() -> String {
    if built_info::GIT_COMMIT_HASH_SHORT.is_some() {
        format!(
            "{}-{}{}",
            built_info::PKG_VERSION,
            built_info::GIT_COMMIT_HASH_SHORT.unwrap(),
            if built_info::GIT_DIRTY.unwrap_or(false) {
                "-dirty"
            } else {
                ""
            }
        )
    } else {
        built_info::PKG_VERSION.to_string()
    }
}
