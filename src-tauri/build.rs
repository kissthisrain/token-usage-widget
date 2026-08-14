fn main() {
    // tauri-build 默认不追踪图标文件变化：图标/托盘图更新后必须重跑本脚本
    // 重新生成并编译 exe 资源（resource.rc → resource.lib），否则 exe 内嵌图标不会刷新
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=../resources/icon.png");
    tauri_build::build()
}
