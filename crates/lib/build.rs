fn main() {
    #[cfg(all(unix, feature = "service"))]
    {
        let schema = "proto/read_v1.proto";
        println!("cargo:rerun-if-changed={schema}");
        let descriptors = protox::compile([schema], ["proto"]).expect("compile read v1 schema");
        tonic_prost_build::configure()
            .bytes(".eidetica.service.v1.ReadFrame.data")
            .compile_fds(descriptors)
            .expect("generate read v1 adapter");
    }
}
