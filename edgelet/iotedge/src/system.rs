// Copyright (c) Microsoft. All rights reserved.

use std::ffi::OsStr;

use lazy_static::lazy_static;

use aziotctl_common::{
    get_status, get_system_logs as logs, restart, set_log_level as log_level, ServiceDefinition,
    SERVICE_DEFINITIONS as IS_SERVICES,
};

use crate::error::{Error, ErrorKind};

lazy_static! {
    static ref IOTEDGED: ServiceDefinition = {
        // Use the presence of IOTEDGE_HOST to infer whether this is being built for CentOS 7 or not.
        // CentOS 7 doesn't use socket activation.
        let sockets: &'static [&'static str] = option_env!("IOTEDGE_HOST").map_or(
            &["aziot-edged.mgmt.socket", "aziot-edged.workload.socket"],
            |_host| &[],
        );

        ServiceDefinition {
            service: "aziot-edged.service",
            sockets,
        }
    };
    static ref SERVICE_DEFINITIONS: Vec<&'static ServiceDefinition> = {
        let iotedged: &ServiceDefinition = &IOTEDGED;

        let service_definitions: Vec<&ServiceDefinition> = std::iter::once(iotedged)
            .chain(IS_SERVICES.iter().copied())
            .collect();

        service_definitions
    };
}

pub struct System;

impl System {
    pub fn get_system_logs(args: &[&OsStr]) -> Result<(), Error> {
        let services: Vec<&str> = SERVICE_DEFINITIONS.iter().map(|s| s.service).collect();

        logs(&services, &args).map_err(|err| {
            eprintln!("{:#?}", err);
            Error::from(ErrorKind::System)
        })
    }

    pub fn system_restart() -> Result<(), Error> {
        restart(&SERVICE_DEFINITIONS).map_err(|err| {
            eprintln!("{:#?}", err);
            Error::from(ErrorKind::System)
        })
    }

    pub fn set_log_level(level: log::Level) -> Result<(), Error> {
        log_level(&SERVICE_DEFINITIONS, level).map_err(|err| {
            eprintln!("{:#?}", err);
            Error::from(ErrorKind::System)
        })
    }

    pub fn get_system_status() -> Result<(), Error> {
        get_status(&SERVICE_DEFINITIONS).map_err(|err| {
            eprintln!("{:#?}", err);
            Error::from(ErrorKind::System)
        })
    }
}
