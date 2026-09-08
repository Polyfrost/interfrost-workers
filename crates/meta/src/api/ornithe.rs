use crate::utils::prelude::*;
use interfrost::api::minecraft::{Argument, ArgumentType, Library};
use interfrost::api::modded::{LoaderVersion, Manifest, PartialVersionInfo, Version};

const META_URL: &str = "https://meta.ornithemc.net/v3/versions";
const MC_VERSIONS_URL: &str = "https://ornithemc.net/mc-versions";
const MAVEN_URL: &str = "https://maven.ornithemc.net/releases/";
const GENERATION: u8 = 2;
const TEMPLATE_GAME_VERSION: &str = "1.8.9";
const INTERMEDIARY_GROUP: &str = "net.ornithemc:calamus-intermediary";
const BATCH_SIZE: usize = 100;

fn escape(game_version: &str) -> String {
	game_version.replace(' ', "%20")
}

#[tracing::instrument(skip(semaphore, upload_files, mirror_artifacts))]
pub async fn fetch(
	semaphore: Arc<Semaphore>,
	upload_files: &crate::UploadFiles,
	mirror_artifacts: &crate::MirrorArtifacts,
) -> crate::utils::Result<()> {
	let format_version = interfrost::api::modded::CURRENT_ORNITHE_FORMAT_VERSION;
	tracing::info!("fetching ornithe (gen{GENERATION}) mod loader metadata!");

	let manifest_path = format!("ornithe/v{format_version}/manifest.json");
	let libraries_path = format!("ornithe/v{format_version}/game-libraries.json");

	let existing_manifest =
		crate::utils::fetch_json::<Manifest>(&crate::utils::format_url(&manifest_path), &semaphore)
			.await
			.ok();
	let loaders = crate::utils::fetch_json::<Vec<OrnitheVersion>>(
		&format!("{META_URL}/gen{GENERATION}/fabric-loader"),
		&semaphore,
	)
	.await?;
	let intermediaries = crate::utils::fetch_json::<Vec<OrnitheVersion>>(
		&format!("{META_URL}/gen{GENERATION}/intermediary"),
		&semaphore,
	)
	.await?;

	let (new_loaders, new_games) =
		changed_versions(existing_manifest.as_ref(), &loaders, &intermediaries);

	if new_loaders.is_empty() && new_games.is_empty() {
		tracing::info!("ornithe metadata is already up to date!");
		return Ok(());
	}

	let mut game_libraries = crate::utils::fetch_json::<HashMap<String, Vec<Library>>>(
		&crate::utils::format_url(&libraries_path),
		&semaphore,
	)
	.await
	.unwrap_or_default();

	let required_games = if new_loaders.is_empty() {
		new_games.clone()
	} else {
		intermediaries.iter().collect()
	};
	let missing_games = required_games
		.iter()
		.filter(|game| !game_libraries.contains_key(&game.version))
		.collect::<Vec<_>>();

	for batch in missing_games.chunks(BATCH_SIZE) {
		let fetched = futures::future::try_join_all(
			batch
				.iter()
				.map(|game| fetch_game_libraries(game, &semaphore, mirror_artifacts)),
		)
		.await?;

		for (game, libraries) in batch.iter().zip(fetched) {
			game_libraries.insert(game.version.clone(), libraries);
		}
	}

	let required_loaders = if new_games.is_empty() {
		new_loaders.clone()
	} else {
		loaders.iter().collect()
	};
	let mut base_profiles = HashMap::new();

	for batch in required_loaders.chunks(BATCH_SIZE) {
		let fetched = futures::future::try_join_all(batch.iter().map(|loader| {
			let url = format!(
				"{META_URL}/gen{GENERATION}/fabric-loader/{TEMPLATE_GAME_VERSION}/{}/profile/json",
				loader.version
			);
			let semaphore = semaphore.clone();

			async move { crate::utils::fetch_json::<PartialVersionInfo>(&url, &semaphore).await }
		}))
		.await?;

		for (loader, mut profile) in batch.iter().zip(fetched) {
			profile
				.libraries
				.retain(|library| !library.name.starts_with(INTERMEDIARY_GROUP));

			for library in &mut profile.libraries {
				mirror_library(library, MAVEN_URL, mirror_artifacts)?;
			}

			base_profiles.insert(loader.version.clone(), profile);
		}
	}

	for game in &intermediaries {
		let loaders = if new_games.iter().any(|x| x.version == game.version) {
			loaders.iter().collect::<Vec<_>>()
		} else {
			new_loaders.clone()
		};

		let Some(libraries) = game_libraries.get(&game.version) else {
			continue;
		};

		for loader in loaders {
			let Some(base) = base_profiles.get(&loader.version) else {
				continue;
			};

			let profile = build_profile(base, game, libraries);
			let version_path = format!(
				"ornithe/v{format_version}/versions/{}/{}.json",
				game.version, loader.version
			);

			upload_files.insert(
				version_path,
				crate::utils::UploadFile {
					file: Bytes::from(serde_json::to_vec(&profile)?),
					content_type: Some("application/json".to_string()),
				},
			);
		}
	}

	upload_files.insert(
		libraries_path,
		crate::utils::UploadFile {
			file: Bytes::from(serde_json::to_vec(&game_libraries)?),
			content_type: Some("application/json".to_string()),
		},
	);

	let manifest = Manifest {
		game_versions: intermediaries
			.into_iter()
			.map(|game| Version {
				id: game.version.clone(),
				stable: game.stable,
				loaders: loaders
					.iter()
					.map(|loader| LoaderVersion {
						id: loader.version.clone(),
						url: crate::utils::format_url(&format!(
							"ornithe/v{format_version}/versions/{}/{}.json",
							escape(&game.version),
							loader.version
						)),
						stable: loader.stable,
					})
					.collect(),
			})
			.collect(),
	};

	upload_files.insert(
		manifest_path,
		crate::utils::UploadFile {
			file: Bytes::from(serde_json::to_vec(&manifest)?),
			content_type: Some("application/json".to_string()),
		},
	);

	Ok(())
}

fn changed_versions<'a>(
	existing: Option<&Manifest>,
	loaders: &'a [OrnitheVersion],
	intermediaries: &'a [OrnitheVersion],
) -> (Vec<&'a OrnitheVersion>, Vec<&'a OrnitheVersion>) {
	let Some(existing) = existing else {
		return (loaders.iter().collect(), intermediaries.iter().collect());
	};

	(
		loaders
			.iter()
			.filter(|loader| {
				!existing
					.game_versions
					.iter()
					.any(|game| game.loaders.iter().any(|x| x.id == loader.version))
			})
			.collect(),
		intermediaries
			.iter()
			.filter(|game| !existing.game_versions.iter().any(|x| x.id == game.version))
			.collect(),
	)
}

#[tracing::instrument(skip(semaphore, mirror_artifacts))]
async fn fetch_game_libraries(
	game: &OrnitheVersion,
	semaphore: &Arc<Semaphore>,
	mirror_artifacts: &crate::MirrorArtifacts,
) -> crate::utils::Result<Vec<Library>> {
	let mut libraries = crate::utils::fetch_json::<Vec<Library>>(
		&format!(
			"{META_URL}/gen{GENERATION}/libraries/{}",
			escape(&game.version)
		),
		semaphore,
	)
	.await?;
	let manifest = crate::utils::fetch_json::<GameVersionManifest>(
		&format!(
			"{MC_VERSIONS_URL}/gen{GENERATION}/version/manifest/{}.json",
			escape(&game.version)
		),
		semaphore,
	)
	.await?;

	libraries.extend(manifest.libraries.into_iter().filter(is_upgraded_lwjgl));

	for library in &mut libraries {
		mirror_library(
			library,
			"https://libraries.minecraft.net/",
			mirror_artifacts,
		)?;
	}

	crate::utils::insert_mirrored_artifact(
		&game.maven,
		None,
		vec![MAVEN_URL.to_string()],
		false,
		mirror_artifacts,
	)?;

	Ok(libraries)
}

fn is_upgraded_lwjgl(library: &Library) -> bool {
	library.name.contains("lwjgl")
		&& library.downloads.as_ref().is_some_and(|downloads| {
			downloads
				.artifact
				.iter()
				.chain(downloads.classifiers.iter().flat_map(|x| x.values()))
				.any(|download| !download.url.starts_with("https://libraries.minecraft.net/"))
		})
}

fn mirror_library(
	library: &mut Library,
	fallback_maven: &str,
	mirror_artifacts: &crate::MirrorArtifacts,
) -> crate::utils::Result<()> {
	if library.downloads.is_some() {
		return Ok(());
	}

	crate::utils::insert_mirrored_artifact(
		&library.name,
		None,
		vec![
			library
				.url
				.clone()
				.unwrap_or_else(|| fallback_maven.to_string()),
		],
		false,
		mirror_artifacts,
	)?;
	library.url = Some(crate::utils::format_url("maven/"));

	Ok(())
}

fn build_profile(
	base: &PartialVersionInfo,
	game: &OrnitheVersion,
	libraries: &[Library],
) -> PartialVersionInfo {
	PartialVersionInfo {
		id: base.id.replace(TEMPLATE_GAME_VERSION, &game.version),
		inherits_from: game.version.clone(),
		release_time: base.release_time,
		time: base.time,
		main_class: base.main_class.clone(),
		minecraft_arguments: base.minecraft_arguments.clone(),
		arguments: Some(retarget_arguments(base.arguments.clone(), &game.version)),
		libraries: base
			.libraries
			.iter()
			.cloned()
			.chain(std::iter::once(Library {
				downloads: None,
				extract: None,
				name: game.maven.clone(),
				url: Some(crate::utils::format_url("maven/")),
				natives: None,
				rules: None,
				checksums: None,
				include_in_classpath: true,
				downloadable: true,
			}))
			.chain(libraries.iter().cloned())
			.collect(),
		type_: base.type_,
		data: None,
		processors: None,
	}
}

fn retarget_arguments(
	arguments: Option<HashMap<ArgumentType, Vec<Argument>>>,
	game_version: &str,
) -> HashMap<ArgumentType, Vec<Argument>> {
	const FLAG: &str = "-Dfabric.gameVersion=";

	let mut arguments = arguments.unwrap_or_default();
	let jvm = arguments.entry(ArgumentType::Jvm).or_default();
	let flag = Argument::Normal(format!("{FLAG}{game_version}"));

	if let Some(existing) = jvm
		.iter_mut()
		.find(|argument| matches!(argument, Argument::Normal(x) if x.starts_with(FLAG)))
	{
		*existing = flag;
	} else {
		jvm.push(flag);
	}

	arguments
}

#[derive(Deserialize, Debug, Clone)]
struct OrnitheVersion {
	pub version: String,
	pub maven: String,
	#[serde(default)]
	pub stable: bool,
}

#[derive(Deserialize, Debug)]
struct GameVersionManifest {
	pub libraries: Vec<Library>,
}

#[cfg(test)]
mod tests {
	use super::*;

	const BASE_PROFILE: &str = r#"{
		"id": "fabric-loader-0.19.5-1.8.9-ornithe-gen2",
		"inheritsFrom": "1.8.9-vanilla",
		"releaseTime": "2026-09-08T08:38:27+0200",
		"time": "2026-09-08T08:38:27+0200",
		"type": "release",
		"mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
		"arguments": {
			"jvm": ["-Dfabric.fixPackageAccess=true", "-Dfabric.gameVersion=1.8.9"],
			"game": []
		},
		"libraries": [{ "name": "net.fabricmc:fabric-loader:0.19.5", "url": "https://maven.fabricmc.net/" }]
	}"#;

	fn base_url() {
		static ONCE: std::sync::Once = std::sync::Once::new();
		ONCE.call_once(|| unsafe { std::env::set_var("BASE_URL", "https://meta.example") });
	}

	fn game(version: &str) -> OrnitheVersion {
		OrnitheVersion {
			version: version.to_string(),
			maven: format!("net.ornithemc:calamus-intermediary-gen2:{version}"),
			stable: true,
		}
	}

	fn jvm_arguments(profile: &PartialVersionInfo) -> Vec<String> {
		profile.arguments.as_ref().unwrap()[&ArgumentType::Jvm]
			.iter()
			.map(|argument| match argument {
				Argument::Normal(x) => x.clone(),
				Argument::Ruled { .. } => panic!("unexpected ruled argument"),
			})
			.collect()
	}

	#[test]
	fn only_versions_the_manifest_is_missing_are_regenerated() {
		let loaders = vec![game("0.19.5"), game("0.19.4")];
		let intermediaries = vec![game("1.8.9"), game("b1.7.3")];
		let names = |versions: Vec<&OrnitheVersion>| {
			versions
				.into_iter()
				.map(|x| x.version.clone())
				.collect::<Vec<_>>()
		};

		let (new_loaders, new_games) = changed_versions(None, &loaders, &intermediaries);
		assert_eq!(names(new_loaders), ["0.19.5", "0.19.4"]);
		assert_eq!(names(new_games), ["1.8.9", "b1.7.3"]);

		let existing = serde_json::from_str::<Manifest>(
			r#"{"gameVersions": [
				{ "id": "1.8.9", "stable": true, "loaders": [
					{ "id": "0.19.4", "url": "https://meta.example/1.8.9/0.19.4.json", "stable": false }
				]}
			]}"#,
		)
		.unwrap();

		let (new_loaders, new_games) = changed_versions(Some(&existing), &loaders, &intermediaries);
		assert_eq!(names(new_loaders), ["0.19.5"]);
		assert_eq!(names(new_games), ["b1.7.3"]);

		let (new_loaders, new_games) =
			changed_versions(Some(&existing), &loaders[1..], &intermediaries[..1]);
		assert!(new_loaders.is_empty() && new_games.is_empty());
	}

	#[test]
	fn a_profile_is_retargeted_at_another_game_version() {
		base_url();
		let base = serde_json::from_str::<PartialVersionInfo>(BASE_PROFILE).unwrap();
		let extra = serde_json::from_str::<Vec<Library>>(
			r#"[{ "name": "com.google.code.gson:gson:2.10", "url": "https://libraries.minecraft.net/" }]"#,
		)
		.unwrap();
		let profile = build_profile(&base, &game("b1.7.3"), &extra);

		assert_eq!(profile.inherits_from, "b1.7.3");
		assert_eq!(profile.id, "fabric-loader-0.19.5-b1.7.3-ornithe-gen2");
		assert_eq!(
			jvm_arguments(&profile),
			[
				"-Dfabric.fixPackageAccess=true",
				"-Dfabric.gameVersion=b1.7.3"
			]
		);

		let libraries = profile
			.libraries
			.iter()
			.map(|library| library.name.as_str())
			.collect::<Vec<_>>();
		assert_eq!(
			libraries,
			[
				"net.fabricmc:fabric-loader:0.19.5",
				"net.ornithemc:calamus-intermediary-gen2:b1.7.3",
				"com.google.code.gson:gson:2.10"
			]
		);
	}

	#[test]
	fn a_profile_without_the_flag_is_given_one() {
		base_url();
		let mut base = serde_json::from_str::<PartialVersionInfo>(BASE_PROFILE).unwrap();
		base.arguments = None;

		let profile = build_profile(&base, &game("1.5.2"), &[]);
		assert_eq!(jvm_arguments(&profile), ["-Dfabric.gameVersion=1.5.2"]);
	}

	#[test]
	fn only_lwjgl_ornithe_replaced_is_taken_from_a_game_version_manifest() {
		let manifest = serde_json::from_str::<GameVersionManifest>(
			r#"{"libraries": [
				{
					"name": "org.lwjgl.lwjgl:lwjgl:2.9.0",
					"downloads": { "artifact": { "sha1": "a", "size": 1, "url": "https://libraries.minecraft.net/org/lwjgl/lwjgl/lwjgl/2.9.0/lwjgl-2.9.0.jar" } }
				},
				{
					"name": "org.lwjgl.lwjgl:lwjgl:2.9.4+legacyfabric.15",
					"downloads": { "artifact": { "sha1": "b", "size": 2, "url": "https://maven.legacyfabric.net/org/lwjgl/lwjgl/lwjgl/2.9.4+legacyfabric.15/lwjgl-2.9.4+legacyfabric.15.jar" } }
				},
				{
					"name": "com.google.guava:guava:17.0",
					"downloads": { "artifact": { "sha1": "c", "size": 3, "url": "https://maven.legacyfabric.net/com/google/guava/guava/17.0/guava-17.0.jar" } }
				}
			]}"#,
		)
		.unwrap();

		let taken = manifest
			.libraries
			.into_iter()
			.filter(is_upgraded_lwjgl)
			.map(|library| library.name)
			.collect::<Vec<_>>();
		assert_eq!(taken, ["org.lwjgl.lwjgl:lwjgl:2.9.4+legacyfabric.15"]);
	}

	#[tokio::test]
	#[ignore = "hits ornithe's servers"]
	async fn live_ornithe_metadata_still_parses() {
		base_url();
		let semaphore = Arc::new(Semaphore::new(10));
		let mirror_artifacts = crate::MirrorArtifacts::new();

		let loaders = crate::utils::fetch_json::<Vec<OrnitheVersion>>(
			&format!("{META_URL}/gen{GENERATION}/fabric-loader"),
			&semaphore,
		)
		.await
		.unwrap();
		let intermediaries = crate::utils::fetch_json::<Vec<OrnitheVersion>>(
			&format!("{META_URL}/gen{GENERATION}/intermediary"),
			&semaphore,
		)
		.await
		.unwrap();

		assert!(!loaders.is_empty() && !intermediaries.is_empty());

		crate::utils::fetch_json::<PartialVersionInfo>(
			&format!(
				"{META_URL}/gen{GENERATION}/fabric-loader/{TEMPLATE_GAME_VERSION}/{}/profile/json",
				loaders[0].version
			),
			&semaphore,
		)
		.await
		.unwrap();

		let awkward = intermediaries
			.iter()
			.find(|game| game.version.contains(' '))
			.expect("a game version with a space in its id");

		for game in intermediaries
			.iter()
			.step_by(40)
			.chain(std::iter::once(awkward))
		{
			fetch_game_libraries(game, &semaphore, &mirror_artifacts)
				.await
				.unwrap_or_else(|err| panic!("{}: {err}", game.version));
		}
	}

	#[test]
	fn a_library_that_names_its_own_files_is_not_mirrored() {
		let mirror_artifacts = crate::MirrorArtifacts::new();
		let mut library = serde_json::from_str::<Library>(
			r#"{
				"name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.4+legacyfabric.15",
				"downloads": { "classifiers": { "natives-osx": { "sha1": "a", "size": 1, "url": "https://maven.legacyfabric.net/lwjgl-platform-natives-osx.jar" } } }
			}"#,
		)
		.unwrap();

		mirror_library(&mut library, MAVEN_URL, &mirror_artifacts).unwrap();
		assert!(mirror_artifacts.is_empty());
		assert!(library.url.is_none());
	}
}
