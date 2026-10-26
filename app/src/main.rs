//! The `openkumite` executable: the game is the library (lib.rs), which Android loads itself (`android_main`).

fn main() -> impl std::process::Termination {
    openkumite::main()
}
