use std::env::consts::ARCH;
use std::ffi::OsString;
use std::{any::Any, path::PathBuf};

use anyhow::{anyhow, Result};
use async_compression::futures::bufread::GzipDecoder;
use async_tar::Archive;
use async_trait::async_trait;
use futures::io::BufReader;
use log::info;
use regex::Regex;

use language::{LanguageServerName, LspAdapter, LspAdapterDelegate};
use lsp::LanguageServerBinary;
use smol::io::AsyncReadExt;

const JDT_MILESTONES_URL: &str = "https://download.eclipse.org";
const JRE_21_MACOS_AARCH64: &'static str =
    "https://corretto.aws/downloads/latest/amazon-corretto-21-aarch64-macos-jdk.tar.gz";
const JRE_21_MACOS_X64: &'static str =
    "https://corretto.aws/downloads/latest/amazon-corretto-21-x64-macos-jdk.tar.gz";

const JAVA_HOME: &'static str = "amazon-corretto-21.jdk/Contents/Home";

pub struct JavaLspAdapter {}

#[async_trait]
impl LspAdapter for JavaLspAdapter {
    fn name(&self) -> LanguageServerName {
        LanguageServerName("java".into())
    }

    fn short_name(&self) -> &'static str {
        "java"
    }

    async fn fetch_latest_server_version(
        &self,
        delegate: &dyn LspAdapterDelegate,
    ) -> Result<Box<dyn 'static + Any + Send>> {
        let mut response = String::new();
        delegate
            .http_client()
            .get(
                format!("{}/jdtls/milestones", JDT_MILESTONES_URL).as_str(),
                Default::default(),
                true,
            )
            .await
            .map_err(|err| anyhow!("Error getting JDT version. {}", err))?
            .body_mut()
            .read_to_string(&mut response)
            .await?;

        let version_re = Regex::new(r"<a href='/jdtls/milestones/(\d+)\.(\d+)\.(\d+)'>")?;

        let mut versions = vec![];
        for (_, [major, minor, patch]) in version_re.captures_iter(&response).map(|c| c.extract()) {
            versions.push(format!(
                "{:0>3}{:0>3}{:0>3}-{}.{}.{}",
                major, minor, patch, major, minor, patch
            ));
        }
        versions.sort();
        let version = &versions[versions.len() - 1][10..];
        info!("Latest version: {}", version);
        Ok(Box::new(version.to_owned()))
    }

    async fn fetch_server_binary(
        &self,
        version: Box<dyn 'static + Send + Any>,
        container_dir: PathBuf,
        delegate: &dyn LspAdapterDelegate,
    ) -> Result<LanguageServerBinary> {
        info!("fetch_server_binary");
        let jdtls_version = version.downcast::<String>().unwrap();

        let jre21_url: &str = match ARCH {
            "aarch64" => JRE_21_MACOS_AARCH64,
            "x86_64" => JRE_21_MACOS_X64,
            _ => "", // meh
        };

        if !container_dir.join(JAVA_HOME).exists() {
            info!("Downloading {}", jre21_url);
            let mut response = delegate
                .http_client()
                .get(jre21_url, Default::default(), true)
                .await
                .map_err(|err| anyhow!("error downloading JRE-21: {}", err))?;
            let decompressed_bytes = GzipDecoder::new(BufReader::new(response.body_mut()));
            let archive = Archive::new(decompressed_bytes);
            archive.unpack(container_dir.clone()).await?;
        }

        if !container_dir
            .join("plugins/org.eclipse.equinox.launcher_1.6.700.v20231214-2017.jar")
            .exists()
        {
            let version_page_url =
                format!("{}/jdtls/milestones/{}", JDT_MILESTONES_URL, jdtls_version);

            let mut response = String::new();
            delegate
                .http_client()
                .get(&version_page_url, Default::default(), true)
                .await
                .map_err(|err| anyhow!("error downloading release: {}", err))?
                .body_mut()
                .read_to_string(&mut response)
                .await?;
            let download_build_re = Regex::new(
                r#"<a href='https://www.eclipse.org/downloads/download.php\?file=(.*\.tar\.gz)'"#,
            )?;

            let build = download_build_re
                .captures(&response)
                .unwrap()
                .get(1)
                .unwrap()
                .as_str();

            let download_url = format!("{}{}", JDT_MILESTONES_URL, build);
            info!("Downloading the JDT-LS from {}", download_url);
            let mut response = delegate
                .http_client()
                .get(&download_url, Default::default(), true)
                .await
                .map_err(|err| anyhow!("error downloading release: {}", err))?;
            let decompressed_bytes = GzipDecoder::new(BufReader::new(response.body_mut()));
            let archive = Archive::new(decompressed_bytes);
            archive.unpack(container_dir.clone()).await?;

            info!("{:?}", container_dir.join("version.txt"));
            std::fs::write(container_dir.join("version.txt"), &*jdtls_version)?;
        }

        let arguments = arguments(&container_dir);
        Ok(LanguageServerBinary {
            path: java(&container_dir),
            arguments,
            env: None,
        })
    }

    async fn cached_server_binary(
        &self,
        container_dir: PathBuf,
        _: &dyn LspAdapterDelegate,
    ) -> Option<LanguageServerBinary> {
        info!("cached_server_binary");

        Some(LanguageServerBinary {
            path: java(&container_dir),
            arguments: arguments(&container_dir),
            env: None,
        })
    }

    async fn installation_test_binary(
        &self,
        container_dir: PathBuf,
    ) -> Option<LanguageServerBinary> {
        info!("installation_test_binary");
        Some(LanguageServerBinary {
            path: java(&container_dir),
            arguments: arguments(&container_dir),
            env: None,
        })
    }

    fn initialization_options(&self) -> Option<serde_json::Value> {
        None
    }
}

fn java(container_dir: &PathBuf) -> PathBuf {
    PathBuf::from(container_dir.join("amazon-corretto-21.jdk/Contents/Home/bin/java"))
}

fn arguments(container_dir: &PathBuf) -> Vec<OsString> {
    let jar = container_dir.join("plugins/org.eclipse.equinox.launcher_1.6.700.v20231214-2017.jar");
    let jar = jar.to_str().unwrap().trim();

    vec![
        "-jar",
        jar,
        "-Declipse.application=org.eclipse.jdt.ls.core.id1",
        "-Dosgi.bundles.defaultStartLevel=4",
        "-Dosgi.checkConfiguration=true",
        "-Declipse.product=org.eclipse.jdt.ls.core.product",
        "-Dosgi.sharedConfiguration.area.readOnly=true",
        "-Dosgi.configuration.cascaded=true",
        "-Xmx1G",
        "--add-modules=ALL-SYSTEM",
        "--add-opens java.base/java.util=ALL-UNNAMED",
        "--add-opens java.base/java.lang=ALL-UNNAMED",
        "-configuration",
        &config(container_dir),
        "-data",
        ".", // here JDT wants the project dir, but does Zed provide it and is it necessary?
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn config(container_dir: &PathBuf) -> String {
    match ARCH {
        "aarch64" => container_dir.join("config_mac").to_str().unwrap().into(),
        "x86_64" => container_dir
            .join("config_mac_arm")
            .to_str()
            .unwrap()
            .into(),
        _ => "".into(), // meh
    }
}
