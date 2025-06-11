fn main() {
    let mut build = cc::Build::new();
    
    build
        .cpp(true) 
        .include("vendor/libutp") 

        .file("vendor/libutp/utp_api.cpp")
        .file("vendor/libutp/utp_hash.cpp")
        .file("vendor/libutp/utp_internal.cpp")
        .file("vendor/libutp/utp_packedsockaddr.cpp")
        .file("vendor/libutp/utp_utils.cpp")
        .file("vendor/libutp/utp_callbacks.cpp")
    ;

    if cfg!(target_os = "windows") {
        println!("cargo:rustc-link-lib=Ws2_32");
    }

    build.compile("libutp");
    println!("cargo:rustc-link-lib=static=libutp");
}