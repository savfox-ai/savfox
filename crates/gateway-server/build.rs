fn main() {
    // Recompile gateway-server whenever the Glory web build updates static/.
    println!("cargo:rerun-if-changed=./static");
}
