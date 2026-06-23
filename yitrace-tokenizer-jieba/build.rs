// 链接团队 cppjieba 包装库（仅 `link` feature）。`mock` 默认下不链接外部库，靠 crate 内 Rust 桩提供符号。
//
// 真库就位时按需调整：库名（vexjieba）、搜索路径（VEXJIEBA_LIB_DIR 环境变量）、是否需要 libstdc++。
fn main() {
    if std::env::var("CARGO_FEATURE_LINK").is_ok() {
        // 库搜索路径：优先环境变量，便于离线/信创构建机指定 vendored 库目录。
        if let Ok(dir) = std::env::var("VEXJIEBA_LIB_DIR") {
            println!("cargo:rustc-link-search=native={dir}");
        }
        // 团队 cppjieba 的 C 包装库（静态优先；动态库去掉 static= 前缀）。
        println!("cargo:rustc-link-lib=static=vexjieba");
        // cppjieba 是 C++，链接 C++ 运行时（macOS: c++；Linux: stdc++）。
        let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        if target == "macos" {
            println!("cargo:rustc-link-lib=dylib=c++");
        } else {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        println!("cargo:rerun-if-env-changed=VEXJIEBA_LIB_DIR");
    }
}
