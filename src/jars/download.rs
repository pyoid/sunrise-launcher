//! Mods' required jars, found on Modrinth by their exact SHA-512 and downloaded from its CDN (from sunrise-port-pack's `modrinth` and `fetch`).
use serde::Deserialize;
use serde_json::Value;
use sha2::{digest::Output, Digest};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::Path,
};
use sunrise_launcher_smod::manifest::JarRef;
use ureq::OrAnyStatus;

use crate::browser::{download::MODRINTH_CDN, modrinth::API};
const AGENT: &str = concat!("sunrise-launcher/", env!("CARGO_PKG_VERSION"));
/// The largest jar read or downloaded.
const MAX_JAR: u64 = 512 << 20;

/// The CDN address of each jar, by its SHA-512, in one exact-hash request (after download consent).
pub fn jar_urls(jars: &[&JarRef]) -> Result<BTreeMap<String, String>, String> {
    let hashes: Vec<_> = jars.iter().map(|jar| jar.sha512()).collect();
    let url = format!("{API}/version_files");
    let response = crate::proxy::agent("jar-version-files", &url, |builder| builder.redirects(0))?
        .post(&url)
        .set("User-Agent", AGENT)
        .set("Content-Type", "application/json")
        .send_string(&serde_json::json!({"hashes": hashes, "algorithm": "sha512"}).to_string())
        .or_any_status()
        .map_err(|e| format!("{url}: {}", e.kind()))?;
    let status = response.status();
    if status >= 300 {
        return Err(crate::i18n::trf(
            "{url}: HTTP {status}",
            &[("url", url.as_str()), ("status", &status.to_string())],
        ));
    }
    let text = response.into_string().map_err(|e| format!("{url}: {e}"))?;
    jar_file_urls(serde_json::from_str(&text).map_err(|e| format!("{url}: {e}"))?)
}

fn jar_file_urls(found: Value) -> Result<BTreeMap<String, String>, String> {
    #[derive(Deserialize)]
    struct Version {
        files: Vec<File>,
    }
    #[derive(Deserialize)]
    struct File {
        hashes: BTreeMap<String, String>,
        url: String,
    }
    let versions: BTreeMap<String, Version> = serde_json::from_value(found).map_err(|e| {
        crate::i18n::trf(
            "Modrinth version files: {error}",
            &[("error", &e.to_string())],
        )
    })?;
    Ok(versions
        .into_iter()
        .filter_map(|(hash, version)| {
            version
                .files
                .into_iter()
                .find(|file| {
                    file.hashes
                        .get("sha512")
                        .is_some_and(|actual| actual.eq_ignore_ascii_case(&hash))
                })
                .map(|file| (hash, file.url))
        })
        .collect())
}

/// Downloads exactly `jar` from Modrinth's CDN to `dest`, published only once its SHA-512 matches.
pub fn download_jar(url: &str, jar: &JarRef, dest: &Path) -> Result<(), String> {
    let cdn = MODRINTH_CDN
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let response = crate::net::get_from_hosts(url, &[cdn])?
        .ok_or(crate::i18n::tr("Modrinth jar not found"))?;
    let part = dest.with_file_name(format!(".sunrise-download-{}.part", std::process::id()));
    let mut file = fs::File::create(&part).map_err(|e| e.to_string())?;
    let result = (|| {
        let hash = hash_reader::<sha2::Sha512>(response.into_reader(), &mut file)
            .map_err(|e| e.to_string())?;
        let got = format!("{hash:x}");
        if !jar.sha512().eq_ignore_ascii_case(&got) {
            return Err(crate::i18n::trf(
                "SHA-512 {got}, the mod says {expected}",
                &[("got", got.as_str()), ("expected", jar.sha512())],
            ));
        }
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&part, dest).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(&part);
    result.map_err(|e| format!("{url}: {e}"))
}

/// Hashes and copies a jar stream, refusing one larger than 512 MiB; [`io::sink()`] when only the digest is needed.
pub fn hash_reader<D: Digest>(
    mut reader: impl Read,
    mut writer: impl Write,
) -> io::Result<Output<D>> {
    let mut hash = D::new();
    let mut size = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        size += n as u64;
        if size > MAX_JAR {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "jar exceeds 512 MiB",
            ));
        }
        hash.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
    }
    Ok(hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_hash_selects_the_file_not_the_versions_primary_jar() {
        let hash = "a".repeat(128);
        let other = "b".repeat(128);
        let found = serde_json::json!({
            hash.clone(): {"files": [
                {"primary": true, "url": "wrong.jar", "hashes": {"sha512": other}},
                {"url": "exact.jar", "hashes": {"sha512": hash}}
            ]},
            other.clone(): {"files": [
                {"url": "other.jar", "hashes": {"sha512": other.to_uppercase()}}
            ]}
        });
        assert_eq!(
            jar_file_urls(found).unwrap(),
            BTreeMap::from([
                (hash.clone(), "exact.jar".into()),
                (other, "other.jar".into())
            ])
        );
        assert!(jar_file_urls(serde_json::json!({})).unwrap().is_empty());
        assert!(jar_file_urls(serde_json::json!({hash.clone(): {"files": "broken"}})).is_err());
    }
}
