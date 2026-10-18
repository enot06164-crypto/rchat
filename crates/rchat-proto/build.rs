//! Сборка схемы протокола.
//!
//! Требуется установленный `protoc` (см. README). Все `.proto` файлы одного
//! пакета `rchat.proto.v1` компилируются в один Rust-модуль.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protos = [
        "proto/rchat/v1/common.proto",
        "proto/rchat/v1/envelope.proto",
        "proto/rchat/v1/auth.proto",
        "proto/rchat/v1/chat.proto",
        "proto/rchat/v1/message.proto",
        "proto/rchat/v1/sync.proto",
    ];
    println!("cargo:rerun-if-changed=proto");
    let mut config = prost_build::Config::new();
    // Поля bytes остаются Vec<u8>, что упрощает работу без bytes::Bytes в API.
    config.compile_protos(&protos, &["proto"])?;
    Ok(())
}
