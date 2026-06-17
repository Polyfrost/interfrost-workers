use crate::api::minecraft::{Argument, ArgumentType, Library, VersionInfo, VersionType};
use crate::utils::prelude::*;

/// The latest version of the format the fabric model structs deserialize to
pub const CURRENT_FABRIC_FORMAT_VERSION: usize = 0;
/// The latest version of the format the fabric model structs deserialize to
pub const CURRENT_FORGE_FORMAT_VERSION: usize = 0;
/// The latest version of the format the quilt model structs deserialize to
pub const CURRENT_QUILT_FORMAT_VERSION: usize = 0;
/// The latest version of the format the neoforge model structs deserialize to
pub const CURRENT_NEOFORGE_FORMAT_VERSION: usize = 0;
/// The latest version of the format the legacy fabric model structs deserialize to
pub const CURRENT_LEGACY_FABRIC_FORMAT_VERSION: usize = 0;
/// The latest version of the format the cleanroom model structs deserialize to
pub const CURRENT_CLEANROOM_FORMAT_VERSION: usize = 0;

/// The dummy replace string library names, inheritsFrom, and version names should be replaced with
pub const DUMMY_REPLACE_STRING: &str = "${interfrost.gameVersion}";

/// A data variable entry that depends on the side of the installation
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SidedDataEntry {
	/// The value on the client
	pub client: String,
	/// The value on the server
	pub server: String,
}

fn deserialize_date<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
where
	D: Deserializer<'de>,
{
	let s = String::deserialize(deserializer)?;
	parse_date(&s).map_err(serde::de::Error::custom)
}

fn parse_date(s: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
	DateTime::parse_from_rfc3339(s)
		.or_else(|_| {
			normalize_short_timezone_offset(s).map_or_else(
				|| DateTime::parse_from_rfc3339(s),
				|s| DateTime::parse_from_rfc3339(&s),
			)
		})
		.map(|date| date.with_timezone(&Utc))
		.or_else(|_| {
			chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.9f")
				.map(|date| date.and_utc())
		})
}

fn normalize_short_timezone_offset(s: &str) -> Option<String> {
	let offset_start = s.rfind(['+', '-'])?;
	let offset = s.as_bytes().get(offset_start..)?;

	if offset.len() == 5
		&& offset[1].is_ascii_digit()
		&& offset[2] == b':'
		&& offset[3].is_ascii_digit()
		&& offset[4].is_ascii_digit()
	{
		let mut normalized = String::with_capacity(s.len() + 1);
		normalized.push_str(&s[..=offset_start]);
		normalized.push('0');
		normalized.push_str(&s[offset_start + 1..]);

		Some(normalized)
	} else {
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_date_accepts_forge_short_utc_offset() {
		let parsed = parse_date("2026-05-27T14:13:59+0:00").unwrap();

		assert_eq!(parsed.to_rfc3339(), "2026-05-27T14:13:59+00:00");
	}

	#[test]
	fn partial_version_info_accepts_forge_short_utc_offset() {
		let parsed: PartialVersionInfo = serde_json::from_str(
			r#"{
				"id": "1.21.11-forge-61.1.7",
				"inheritsFrom": "1.21.11",
				"releaseTime": "2026-05-27T14:13:59+0:00",
				"time": "2026-05-27T14:13:59+0:00",
				"libraries": [],
				"type": "release"
			}"#,
		)
		.unwrap();

		assert_eq!(parsed.time.to_rfc3339(), "2026-05-27T14:13:59+00:00");
		assert_eq!(
			parsed.release_time.to_rfc3339(),
			"2026-05-27T14:13:59+00:00"
		);
	}

	#[test]
	fn parse_date_keeps_existing_formats() {
		assert_eq!(
			parse_date("2026-05-27T14:13:59Z").unwrap().to_rfc3339(),
			"2026-05-27T14:13:59+00:00"
		);
		assert_eq!(
			parse_date("2026-05-27T14:13:59.123").unwrap().to_rfc3339(),
			"2026-05-27T14:13:59.123+00:00"
		);
	}
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
/// A partial version returned by fabric meta
pub struct PartialVersionInfo {
	/// The version ID of the version
	pub id: String,
	/// The version ID this partial version inherits from
	pub inherits_from: String,
	/// The time that the version was released
	#[serde(deserialize_with = "deserialize_date")]
	pub release_time: DateTime<Utc>,
	/// The latest time a file in this version was updated
	#[serde(deserialize_with = "deserialize_date")]
	pub time: DateTime<Utc>,
	#[serde(skip_serializing_if = "Option::is_none")]
	/// The classpath to the main class to launch the game
	pub main_class: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	/// (Legacy) Arguments passed to the game
	pub minecraft_arguments: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	/// Arguments passed to the game or JVM
	pub arguments: Option<HashMap<ArgumentType, Vec<Argument>>>,
	/// Libraries that the version depends on
	pub libraries: Vec<Library>,
	#[serde(rename = "type")]
	/// The type of version
	pub type_: VersionType,
	#[serde(skip_serializing_if = "Option::is_none")]
	/// (Forge-only)
	pub data: Option<HashMap<String, SidedDataEntry>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	/// (Forge-only) The list of processors to run after downloading the files
	pub processors: Option<Vec<Processor>>,
}

/// A processor to be ran after downloading the files
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Processor {
	/// Maven coordinates for the JAR library of this processor.
	pub jar: String,
	/// Maven coordinates for all the libraries that must be included in classpath when running this processor.
	pub classpath: Vec<String>,
	/// Arguments for this processor.
	pub args: Vec<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	/// Represents a map of outputs. Keys and values can be data values
	pub outputs: Option<HashMap<String, String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	/// Which sides this processor shall be ran on.
	/// Valid values: client, server, extract
	pub sides: Option<Vec<String>>,
}

/// Merges a partial version into a complete one
#[must_use]
pub fn merge_partial_version(partial: PartialVersionInfo, merge: VersionInfo) -> VersionInfo {
	let merge_id = merge.id.clone();
	let mut libraries = vec![];

	for mut lib in merge.libraries {
		let lib_artifact = lib.name.rsplit_once(':').map(|x| x.0);
		if let Some(lib_artifact) = lib_artifact {
			if partial.libraries.iter().any(|x| {
				let target_artifact = x.name.rsplit_once(':').map(|x| x.0);
				target_artifact == Some(lib_artifact) && x.include_in_classpath
			}) {
				lib.include_in_classpath = false;
			} else {
				libraries.push(lib);
			}
		} else {
			libraries.push(lib);
		}
	}

	VersionInfo {
		arguments: if let Some(partial_args) = partial.arguments {
			if let Some(merge_args) = merge.arguments {
				fn add_keys(
					new_map: &mut HashMap<ArgumentType, Vec<Argument>>,
					args: HashMap<ArgumentType, Vec<Argument>>,
				) {
					for (type_, arguments) in args {
						for arg in arguments {
							if let Some(vec) = new_map.get_mut(&type_) {
								vec.push(arg);
							} else {
								new_map.insert(type_, vec![arg]);
							}
						}
					}
				}

				let mut new_map = HashMap::new();
				add_keys(&mut new_map, merge_args);
				add_keys(&mut new_map, partial_args);

				Some(new_map)
			} else {
				Some(partial_args)
			}
		} else {
			merge.arguments
		},
		asset_index: merge.asset_index,
		assets: merge.assets,
		downloads: merge.downloads,
		id: partial.id.replace(DUMMY_REPLACE_STRING, &merge_id),
		java_version: merge.java_version,
		libraries: libraries
			.into_iter()
			.chain(partial.libraries)
			.map(|mut x| {
				x.name = x.name.replace(DUMMY_REPLACE_STRING, &merge_id);
				x
			})
			.collect::<Vec<_>>(),
		main_class: if let Some(main_class) = partial.main_class {
			main_class
		} else {
			merge.main_class
		},
		logging: merge.logging,
		minecraft_arguments: partial.minecraft_arguments,
		minimum_launcher_version: merge.minimum_launcher_version,
		release_time: partial.release_time,
		time: partial.time,
		type_: partial.type_,
		data: partial.data,
		processors: partial.processors,
	}
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
/// A manifest containing information about a mod loader's versions
pub struct Manifest {
	/// The game versions the mod loader supports
	pub game_versions: Vec<Version>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Serialize, Deserialize, Debug, Clone)]
///  A game version of Minecraft
pub struct Version {
	/// The minecraft version ID
	pub id: String,
	/// Whether the release is stable or not
	pub stable: bool,
	/// A map that contains loader versions for the game version
	pub loaders: Vec<LoaderVersion>,
}

#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Serialize, Deserialize, Debug, Clone)]
/// A version of a Minecraft mod loader
pub struct LoaderVersion {
	/// The version ID of the loader
	pub id: String,
	/// The URL of the version's manifest
	pub url: String,
	/// Whether the loader is stable or not
	pub stable: bool,
}
