use std::time::Duration;

use anyhow::{anyhow, Context};
use regex::Regex;
use semver::Version;

use crate::check::{Check, CheckResult, Checker, CheckerMeta};
use crate::error::{Error, FetchLatestVersionsReason};

const AKA_MS_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Default, serde::Serialize)]
pub(crate) struct AziotEdgedVersion {
    actual_version: Option<String>,
    expected_version: Option<String>,
}

#[async_trait::async_trait]
impl Checker for AziotEdgedVersion {
    fn meta(&self) -> CheckerMeta {
        CheckerMeta {
            id: "aziot-edge-version",
            description: "aziot-edge package is up-to-date",
        }
    }

    async fn execute(&mut self, check: &mut Check) -> CheckResult {
        self.inner_execute(check)
            .await
            .unwrap_or_else(CheckResult::Failed)
    }
}

impl AziotEdgedVersion {
    const URI: &'static str = "https://aka.ms/azure-iotedge-latest-versions";

    async fn get_latest_released_versions(
        &mut self,
        check: &Check,
    ) -> anyhow::Result<crate::LatestVersions> {
        let proxy = check
            .proxy_uri
            .as_ref()
            .map(|proxy| proxy.parse::<hyper::Uri>())
            .transpose()
            .context(Error::FetchLatestVersions(
                FetchLatestVersionsReason::CreateClient,
            ))?;

        let connector = http_common::MaybeProxyConnector::new(proxy, None, &[])
            .context("could not initialize HTTP connector")?;
        let client: hyper::Client<_, hyper::Body> = hyper::Client::builder().build(connector);

        let mut uri: hyper::Uri = Self::URI
            .parse()
            .expect("hard-coded URI cannot fail to parse");
        let latest_versions: crate::LatestVersions = loop {
            let req = {
                let mut req = hyper::Request::new(Default::default());
                *req.uri_mut() = uri.clone();
                req
            };

            let res = tokio::time::timeout(AKA_MS_HTTP_REQUEST_TIMEOUT, client.request(req)).await;
            let res = match res {
                Ok(Ok(res)) => res,
                Ok(Err(e)) => {
                    return Err(e).context(Error::FetchLatestVersions(
                        FetchLatestVersionsReason::GetResponse,
                    ));
                }
                Err(e) => {
                    return Err(e).context(Error::FetchLatestVersions(
                        FetchLatestVersionsReason::RequestTimeout,
                    ));
                }
            };

            match res.status() {
                status_code if status_code.is_redirection() => {
                    uri = res
                        .headers()
                        .get(hyper::header::LOCATION)
                        .ok_or(Error::FetchLatestVersions(
                            FetchLatestVersionsReason::InvalidOrMissingLocationHeader,
                        ))?
                        .to_str()
                        .map_err(|_| {
                            Error::FetchLatestVersions(
                                FetchLatestVersionsReason::InvalidOrMissingLocationHeader,
                            )
                        })?
                        .parse()
                        .map_err(|_| {
                            Error::FetchLatestVersions(
                                FetchLatestVersionsReason::InvalidOrMissingLocationHeader,
                            )
                        })?;
                }

                hyper::StatusCode::OK => {
                    let body = hyper::body::aggregate(res.into_body())
                        .await
                        .context("could not read HTTP response")?;
                    let body = serde_json::from_reader(hyper::body::Buf::reader(body))
                        .context("could not read HTTP response")?;
                    break body;
                }

                status_code => {
                    return Err(Error::FetchLatestVersions(
                        FetchLatestVersionsReason::ResponseStatusCode(status_code),
                    )
                    .into())
                }
            }
        };

        Ok(latest_versions)
    }

    async fn get_installed_version(&mut self, check: &Check) -> Result<String, anyhow::Error> {
        let mut process = tokio::process::Command::new(&check.aziot_edged);
        process.arg("--version");

        let output = process
            .output()
            .await
            .context("Could not spawn aziot-edged process")?;
        if !output.status.success() {
            return Err(anyhow!(
                "aziot-edged returned {}, stderr = {}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
            )
            .context("Could not spawn aziot-edged process"));
        }
        let output = String::from_utf8(output.stdout)
            .context("Could not parse output of aziot-edged --version")?;
        let aziot_edged_version_regex = Regex::new(r"^aziot-edged ([^ ]+)(?: \(.*\))?$")
            .expect("This hard-coded regex is expected to be valid.");
        let captures = aziot_edged_version_regex
            .captures(output.trim())
            .ok_or_else(|| {
                anyhow!("output {:?} does not match expected format", output,)
                    .context("Could not parse output of aziot-edged --version")
            })?;
        let version = captures
            .get(1)
            .expect("unreachable: regex defines one capturing group")
            .as_str();
        Ok(version.to_owned())
    }

    async fn inner_execute(&mut self, check: &mut Check) -> anyhow::Result<CheckResult> {
        let actual_version = self.get_installed_version(check).await?;
        let expected_version = if let Some(expected_aziot_edged_version) =
            &check.expected_aziot_edged_version
        {
            expected_aziot_edged_version.clone()
        } else {
            if check.parent_hostname.is_some() {
                // This is a nested Edge device so it may not be able to access aka.ms or github.com.
                // In the best case the request would be blocked immediately, but in the worst case it may take a long time to time out.
                // The user didn't provide the `expected_aziot_edged_version` param on the CLI, so we just ignore this check.
                return Ok(CheckResult::Ignored);
            }

            // Nightly builds of debian packages have a '~' which isn't allowed in semver. We're only interested in
            // parsing the major and minor versions anyway, so we can just truncate the version string.
            let actual_semver = Version::parse(
                actual_version
                    .split(|c| c == '-' || c == '~')
                    .next()
                    .unwrap_or(&actual_version),
            )
            .context(format!(
                "could not parse actual version {actual_version} as semver"
            ))?;
            let versions: Vec<String> = self
                .get_latest_released_versions(check)
                .await?
                .channels
                .iter()
                .flat_map(|channel| channel.products.iter())
                .filter(|product| product.id == "aziot-edge")
                .flat_map(|product| product.components.iter())
                .filter(|component| component.name == "aziot-edge")
                .map(|component| component.version.clone())
                .collect();
            let parsed_versions = versions
                .iter()
                .map(|version| {
                    Version::parse(version).context("could not parse expected version as semver")
                })
                .collect::<Result<Vec<_>, anyhow::Error>>()?;
            let expected_version = parsed_versions
                .iter()
                .find(|semver| semver.major == actual_semver.major && semver.minor == actual_semver.minor)
                .ok_or_else(|| {
                    anyhow!(
                        "could not find aziot-edge version {}.{}.x in list of supported products at {}",
                        actual_semver.major,
                        actual_semver.minor,
                        Self::URI
                    )
                })?;
            expected_version.to_string()
        };

        self.expected_version = Some(expected_version.clone());
        self.actual_version = Some(actual_version.clone());

        check.additional_info.aziot_edged_version = Some(actual_version.clone());

        if actual_version != expected_version {
            return Ok(CheckResult::Warning(
            anyhow!(
                "Installed IoT Edge daemon has version {} but {} is the latest stable version available.\n\
                 Please see https://aka.ms/iotedge-update-runtime for update instructions.",
                actual_version, expected_version,
            ),
        ));
        }

        Ok(CheckResult::Ok)
    }
}
