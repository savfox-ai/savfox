fn main() {
    // Recompile gateway-server whenever the Dioxus web build updates static/.
    println!("cargo:rerun-if-changed=./static");
}
