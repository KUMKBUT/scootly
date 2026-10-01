//! Protobuf/gRPC-контракты между сервисами.
//! Схемы — в `proto/`, сгенерированный код коммитится в `src/generated/`
//! (перегенерация — `cargo build -p proto`).

pub mod scootly {
    pub mod scooter {
        pub mod v1 {
            #![allow(clippy::all)]
            include!("generated/scootly.scooter.v1.rs");
        }
    }
    pub mod payment {
        pub mod v1 {
            #![allow(clippy::all)]
            include!("generated/scootly.payment.v1.rs");
        }
    }
}
