//! Кодогенерация protobuf/gRPC. protoc вендорится через protoc-bin-vendored,
//! чтобы сборка не зависела от системного protoc (CI/Docker/локальная машина).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    std::fs::create_dir_all("src/generated")?;
    tonic_prost_build::configure()
        .out_dir("src/generated")
        .compile_protos(
            &[
                "proto/scootly/scooter/v1/scooter.proto",
                "proto/scootly/payment/v1/payment.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}
