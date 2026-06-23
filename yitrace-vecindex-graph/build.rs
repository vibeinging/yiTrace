// 链接团队 graph_index 库（仅 `link` feature）。mock 默认下不链接，靠 crate 内 Rust 桩提供符号。
// 真库就位时按需调整：库名（vexgraph）、搜索路径（VEXGRAPH_LIB_DIR）、是否需要 C++ 运行时/BLAS。
fn main() {
    if std::env::var("CARGO_FEATURE_LINK").is_ok() {
        if let Ok(dir) = std::env::var("VEXGRAPH_LIB_DIR") {
            println!("cargo:rustc-link-search=native={dir}");
        }
        println!("cargo:rustc-link-lib=static=vexgraph");
        let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        if target == "macos" {
            println!("cargo:rustc-link-lib=dylib=c++");
        } else {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        println!("cargo:rerun-if-env-changed=VEXGRAPH_LIB_DIR");
    }
}
