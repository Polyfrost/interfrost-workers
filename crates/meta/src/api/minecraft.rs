use crate::utils::{download_file, fetch_json, format_url, sha1_async};
use interfrost::api::minecraft::{
	Argument, ArgumentType, Library, VERSION_MANIFEST_URL, VersionInfo, VersionManifest,
	merge_partial_library,
};
use meta_patcher::patch::LibraryPatch;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

const LIBRARY_PATCHES: &[u8] = include_bytes!("../../library.json");

/// Works around CVE-2026-114938, an out-of-bounds write in `HotSpot`'s AVX-512 `Base64` decoder
pub const DISABLE_BASE64_INTRINSICS: &str = "-XX:-UseBASE64Intrinsics";
const BASE64_INTRINSICS_MIN_JAVA: u32 = 17;

pub fn disable_base64_intrinsics(
	arguments: &mut HashMap<ArgumentType, Vec<Argument>>,
	java_major: u32,
) {
	if java_major < BASE64_INTRINSICS_MIN_JAVA {
		return;
	}
	let jvm = arguments.entry(ArgumentType::Jvm).or_default();
	if !jvm
		.iter()
		.any(|x| matches!(x, Argument::Normal(x) if x == DISABLE_BASE64_INTRINSICS))
	{
		jvm.push(Argument::Normal(DISABLE_BASE64_INTRINSICS.to_string()));
	}
}

#[tracing::instrument(skip(semaphore, upload_files, final_upload_files, _mirror_artifacts))]
pub async fn fetch(
	semaphore: Arc<Semaphore>,
	upload_files: &crate::UploadFiles,
	final_upload_files: &crate::UploadFiles,
	_mirror_artifacts: &crate::MirrorArtifacts,
) -> crate::utils::Result<()> {
	tracing::info!("fetching minecraft metadata!");
	let existing_manifest = fetch_json::<VersionManifest>(
		&format_url(&format!(
			"minecraft/v{}/manifest.json",
			interfrost::api::minecraft::CURRENT_FORMAT_VERSION
		)),
		&semaphore,
	)
	.await
	.ok();
	let mojang_manifest = fetch_json::<VersionManifest>(VERSION_MANIFEST_URL, &semaphore).await?;

	let patches_hash_path = format!(
		"minecraft/v{}/library-patches-sha1.json",
		interfrost::api::minecraft::CURRENT_FORMAT_VERSION
	);
	// the JVM flag is part of the hash so changing it regenerates every cached version
	let patches_hash = sha1_async(bytes::Bytes::from(
		[LIBRARY_PATCHES, DISABLE_BASE64_INTRINSICS.as_bytes()].concat(),
	))
	.await?;
	let patches_unchanged = fetch_json::<String>(&format_url(&patches_hash_path), &semaphore)
		.await
		.is_ok_and(|x| x == patches_hash);

	// TODO: experimental snapshots: https://github.com/PrismLauncher/meta/blob/main/meta/common/mojang-minecraft-experiments.json
	// TODO: old snapshots: https://github.com/PrismLauncher/meta/blob/main/meta/common/mojang-minecraft-old-snapshots.json

	// we check our own minecraft manifest and compare them to mojang's, as to avoid unnecessary processing.
	let (fetch_versions, existing_versions) = if let Some(mut existing_manifest) = existing_manifest
	{
		let (mut fetch_versions, mut existing_versions) = (Vec::new(), Vec::new());

		for version in mojang_manifest.versions {
			if let Some(index) = existing_manifest
				.versions
				.iter()
				.position(|x| x.id == version.id)
			{
				let existing_version = existing_manifest.versions.remove(index);

				if patches_unchanged
					&& existing_version
						.original_sha1
						.as_ref()
						.is_some_and(|x| x == &version.sha1)
				{
					existing_versions.push(existing_version);
				} else {
					fetch_versions.push(version);
				}
			} else {
				fetch_versions.push(version);
			}
		}

		(fetch_versions, existing_versions)
	} else {
		(mojang_manifest.versions, Vec::new())
	};

	if !fetch_versions.is_empty() {
		let version_manifests = futures::future::try_join_all(
			fetch_versions
				.iter()
				.map(|x| download_file(&x.url, Some(&x.sha1), &semaphore)),
		)
		.await?
		.into_iter()
		.map(|x| serde_json::from_slice(&x))
		.collect::<Result<Vec<VersionInfo>, serde_json::Error>>()?;

		// we patch certain libraies of Minecraft versions for apple silicon support, linux compat, and other bugfixes that
		// can be fixed by simply replacing one library with an api-identical patched library.
		let library_patches = fetch_library_patches()?;
		let patched_version_manifests = version_manifests
			.into_iter()
			.map(|mut x| {
				if !library_patches.is_empty() {
					let mut new_libraries = Vec::new();
					for library in x.libraries {
						let mut libs = patch_library(&library_patches, library);
						new_libraries.append(&mut libs);
					}
					x.libraries = new_libraries;
				}

				if let Some(java) = &x.java_version {
					disable_base64_intrinsics(
						x.arguments.get_or_insert_default(),
						java.major_version,
					);
				}

				x
			})
			.collect::<Vec<_>>();

		let serialized_version_manifests = patched_version_manifests
			.iter()
			.map(|x| serde_json::to_vec(x).map(bytes::Bytes::from))
			.collect::<Result<Vec<_>, serde_json::Error>>()?;
		let hashes_version_manifests = futures::future::try_join_all(
			serialized_version_manifests
				.iter()
				.map(|x| sha1_async(x.clone())),
		)
		.await?;

		// uploads the new version manifest and adds it to hhe version manifest
		let mut new_versions = patched_version_manifests
			.into_iter()
			.zip(serialized_version_manifests.into_iter())
			.zip(hashes_version_manifests.into_iter())
			.map(|((version, bytes), hash)| {
				let version_path = format!(
					"minecraft/v{}/versions/{}.json",
					interfrost::api::minecraft::CURRENT_FORMAT_VERSION,
					version.id
				);

				let url = format_url(&version_path);
				upload_files.insert(
					version_path,
					crate::utils::UploadFile {
						file: bytes,
						content_type: Some("application/json".to_string()),
					},
				);

				interfrost::api::minecraft::Version {
					original_sha1: fetch_versions
						.iter()
						.find(|x| x.id == version.id)
						.map(|x| x.sha1.clone()),
					id: version.id,
					type_: version.type_,
					url,
					time: version.time,
					release_time: version.release_time,
					sha1: hash,
					compliance_level: 1,
				}
			})
			.chain(existing_versions.into_iter())
			.collect::<Vec<_>>();

		new_versions.sort_by(|a, b| b.release_time.cmp(&a.release_time));

		let version_manifest_path = format!(
			"minecraft/v{}/manifest.json",
			interfrost::api::minecraft::CURRENT_FORMAT_VERSION
		);

		let new_manifest = VersionManifest {
			latest: mojang_manifest.latest,
			versions: new_versions,
		};

		upload_files.insert(
			version_manifest_path,
			crate::utils::UploadFile {
				file: bytes::Bytes::from(serde_json::to_vec(&new_manifest)?),
				content_type: Some("application/json".to_string()),
			},
		);
		final_upload_files.insert(
			patches_hash_path,
			crate::utils::UploadFile {
				file: bytes::Bytes::from(serde_json::to_vec(&patches_hash)?),
				content_type: Some("application/json".to_string()),
			},
		);
	}

	Ok(())
}

/// includes and parses an array of [`LibraryPatch`] from `../../library.json`.
///
/// note: this uses the [`include_bytes`] macro, be careful of the file size of `library.json`.
fn fetch_library_patches() -> crate::utils::Result<Vec<LibraryPatch>> {
	Ok(serde_json::from_slice(LIBRARY_PATCHES)?)
}

/// patches a [`Library`] based on a ruleset defined in an array of [`LibraryPatch`]es.
pub fn patch_library(patches: &Vec<LibraryPatch>, mut library: Library) -> Vec<Library> {
	let mut val = Vec::new();

	let actual_patches = patches
		.iter()
		.filter(|x| x.match_.contains(&library.name))
		.collect::<Vec<_>>();

	if actual_patches.is_empty() {
	} else {
		for patch in actual_patches {
			if let Some(override_) = &patch.override_ {
				library = merge_partial_library(override_.clone(), library);
			}

			if let Some(additional_libraries) = &patch.additional_libraries {
				for additional_library in additional_libraries {
					if patch.patch_additional_libraries.unwrap_or(false) {
						let mut libs = patch_library(patches, additional_library.clone());
						val.append(&mut libs);
					} else {
						val.push(additional_library.clone());
					}
				}
			}
		}
	}

	val.push(library);

	val
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn library_patches_parse() {
		assert!(!fetch_library_patches().unwrap().is_empty());
	}

	#[test]
	fn base64_intrinsics_disabled_only_from_java_17() {
		let jvm = |major| {
			let mut args = HashMap::new();
			disable_base64_intrinsics(&mut args, major);
			disable_base64_intrinsics(&mut args, major);
			args.remove(&ArgumentType::Jvm)
				.unwrap_or_default()
				.into_iter()
				.map(|x| match x {
					Argument::Normal(x) => x,
					Argument::Ruled { .. } => unreachable!(),
				})
				.collect::<Vec<_>>()
		};
		assert!(jvm(8).is_empty());
		assert!(jvm(16).is_empty());
		assert_eq!(jvm(17), [DISABLE_BASE64_INTRINSICS]);
		assert_eq!(jvm(25), [DISABLE_BASE64_INTRINSICS]);
	}
}
