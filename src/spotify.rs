use futures::{pin_mut, TryStreamExt};
use librespot::core::authentication::Credentials;
use librespot::core::cache::Cache;
use librespot::core::config::SessionConfig;
use librespot::core::session::Session;
use rspotify::clients::BaseClient;
use rspotify::model::{
	AlbumId, ArtistId, FullAlbum, FullArtist, FullPlaylist, FullTrack, PlayableItem, PlaylistId,
	SearchResult, SearchType, SimplifiedAlbum, SimplifiedTrack, TrackId,
};
use rspotify::ClientCredsSpotify;
use rspotify::Credentials as ClientCredentials;
use std::fmt;
use std::path::Path;
use url::Url;

use crate::error::SpotifyError;

use rspotify::ClientResult;
use std::collections::HashMap;
use rspotify::http::{BaseHttpClient, Query};
use rspotify::model::Market;
use serde::Deserialize;

pub fn build_map_cpy<'key, 'value, const N: usize>(
    array: [(&'key str, Option<&'value str>); N],
) -> HashMap<&'key str, &'value str> {
    // Use a manual for loop instead of iterators so we can call `with_capacity`
    // and avoid reallocating.
    let mut map = HashMap::with_capacity(N);
    for (key, value) in array {
        if let Some(value) = value {
            map.insert(key, value);
        }
    }
    map
}

pub(crate) fn convert_result_cpy<'a, T: Deserialize<'a>>(input: &'a str) -> ClientResult<T> {
    serde_json::from_str::<T>(input).map_err(Into::into)
}

pub struct Spotify {
	// librespotify sessopm
	pub session: Session,
	pub spotify: ClientCredsSpotify,
}

impl Spotify {
	/// Create new instance
	pub async fn new(
		username: &str,
		password: &str,
		client_id: &str,
		client_secret: &str,
	) -> Result<Spotify, SpotifyError> {
		// librespot
		let credentials = Credentials::with_password(username, password);
		let (session, _) = Session::connect(
			SessionConfig::default(),
			credentials,
			Some(Cache::new(Some(Path::new("credentials_cache")), None, None, None).unwrap()),
			true,
		)
		.await?;

		// rspotify
		let credentials = ClientCredentials {
			id: client_id.to_string(),
			secret: Some(client_secret.to_string()),
		};
		let spotify = ClientCredsSpotify::new(credentials);
		spotify.request_token().await?;

		Ok(Spotify { session, spotify })
	}

	/// Parse URI or URL into URI
	pub fn parse_uri(uri: &str) -> Result<String, SpotifyError> {
		// Already URI
		if uri.starts_with("spotify:") {
			if uri.split(':').count() < 3 {
				return Err(SpotifyError::InvalidUri);
			}
			return Ok(uri.to_string());
		}

		// Parse URL
		let url = Url::parse(uri)?;
		// Spotify Web Player URL
		if url.host_str() == Some("open.spotify.com") {
			let path = url
				.path_segments()
				.ok_or_else(|| SpotifyError::Error("Missing URL path".into()))?
				.collect::<Vec<&str>>();
			if path.len() < 2 {
				return Err(SpotifyError::InvalidUri);
			}
			return Ok(format!("spotify:{}:{}", path[0], path[1]));
		}
		Err(SpotifyError::InvalidUri)
	}

	/// Fetch data for URI
	pub async fn resolve_uri(&self, uri: &str) -> Result<SpotifyItem, SpotifyError> {
		let parts = uri.split(':').skip(1).collect::<Vec<&str>>();
		let id = parts[1];
		match parts[0] {
			"track" => {
				let track = self
					.spotify
					.track(TrackId::from_id(id).unwrap(), None)
					.await?;
				Ok(SpotifyItem::Track(track))
			}
			"playlist" => {
				let playlist = self
					.spotify
					.playlist(PlaylistId::from_id(id).unwrap(), None, None)
					.await?;
				Ok(SpotifyItem::Playlist(playlist))
			}
			"album" => {
				let album = self
					.spotify
					.album(AlbumId::from_id(id).unwrap(), None)
					.await?;
				Ok(SpotifyItem::Album(album))
			}
			"artist" => {
				let artist = self.spotify.artist(ArtistId::from_id(id).unwrap()).await?;
				Ok(SpotifyItem::Artist(artist))
			}
			// Unsupported / Unimplemented
			_ => Ok(SpotifyItem::Other(uri.to_string())),
		}
	}

	/// Get search results for query
	pub async fn search(&self, query: &str) -> Result<Vec<FullTrack>, SpotifyError> {
		Ok(self
			.spotify
			.search(query, SearchType::Track, None, None, Some(50), Some(0))
			.await
			.map(|result| match result {
				SearchResult::Tracks(page) => page.items,
				_ => Vec::new(),
			})?)
	}

	/// Get all tracks from playlist
	pub async fn full_playlist(&self, id: &str) -> Result<Vec<FullTrack>, SpotifyError> {
		// This is to get the entire playlist instead of just the first 100, as that is what the first request gives you to start with
		let playlist = self // store playlist information for later
			.spotify
			.playlist(PlaylistId::from_id(id).unwrap(), None, None)
			.await?;
		let total_tracks = playlist.tracks.total; // Total number of tracks in playlist
		let mut collected = playlist // The collection of tracks in memory (list gotten so far)
			.tracks
			.items
			.into_iter()
			.filter_map(|item| item.track)
			.flat_map(|p_item| match p_item {
				PlayableItem::Track(track) => Some(track),
				_ => None,
			}).collect::<Vec<FullTrack>>();

		let mut attempts = 1; // Track number of requests

		// If the playlist is less than 100 tracks, no need to loop for more
		if playlist.tracks.next != None{
			let mut _next = playlist
				.tracks
				.next
				.unwrap();
			// While the queue doesn't have all of the songs
			while collected.len() < total_tracks.try_into().unwrap() {
				attempts = attempts + 1;

				// HTTP request for next 100 tracks
				// Setup
				let fields: Option<&str> = None;
				let market: Option<Market> = None;
				let params: HashMap<&str, &str> = build_map_cpy([
					("fields", fields),
					("market", market.map(Into::into))
					]);
				let payload: &Query<'_> = &params;
				let headers = self
					.spotify
					.auth_headers()
					.await?;
				// Request and result
				let mut result: String = self.
					spotify
					.get_http()
					.get(&_next, Some(&headers), payload)
					.await
					.unwrap();

				// This is to modify the response of the playlists track offset/limit request
				// to be compliant for the JSON parsing that is expected for the FullPlaylist object
				let tracks_temp = "{\"tracks\":";
				result = tracks_temp.to_owned() + &result + 
					", \"collaborative\" : " + &playlist.collaborative.to_string() +
					", \"external_urls\": {" + "" + "}," + // TODO: add playlist's external_urls (no neat .to_string() method)
					" \"followers\": {" +
						"\"total\": " + &playlist.followers.total.to_string() + "}," + 
					" \"id\": \"" + &id.to_string() + "\"," + 
					"\"images\": [" + "" +"], " + // TODO: add playlist's images information (no neat .to_string() method)
					"\"name\": \"" + &playlist.name.to_string() + "\"," +
					"\"owner\": {" + // TODO: add playlist's owner information (no neat .to_string() method, and move issues)
						"\"display_name\": \"" + "" + "\"," + 
						" \"external_urls\":{" + "" + "}," + 
						" \"href\":\"" + "" +"\"," + 
						" \"id\":\"" + "" + "\"," + 
						" \"images\": [" + "" + "]}," + 
					" \"snapshot_id\": \"" + &playlist.snapshot_id.to_string() + "\"," + 
					" \"href\": \"" + &_next +"\"}";

        		let new_collect: ClientResult<FullPlaylist> = convert_result_cpy(&result); // The collection of tracks received from the next request
				let modify = new_collect?; // a copy that we can modify
				// The final response of the next item will have nothing, so don't unwrap
				if modify.tracks.next != None {
					_next = modify.tracks.next.unwrap();
				}
				let mut act_collect = modify
					.tracks
					.items
					.into_iter()
					.filter_map(|item| item.track)
					.flat_map(|p_item| match p_item {
						PlayableItem::Track(track) => Some(track),
						_ => None,
					}).collect::<Vec<FullTrack>>();
				collected.append(&mut act_collect);
			}
		}	
		println!("Found {} total songs to be downloaded, with {} put into the queue, and required {} requests", total_tracks, collected.len(), attempts);
		Ok(collected)
	}

	/// Get all tracks from album
	pub async fn full_album(&self, id: &str) -> Result<Vec<SimplifiedTrack>, SpotifyError> {
		Ok(self
			.spotify
			.album(AlbumId::from_id(id).unwrap(), None)
			.await?
			.tracks
			.items)
	}

	/// Get all tracks from artist
	pub async fn full_artist(&self, id: &str) -> Result<Vec<SimplifiedTrack>, SpotifyError> {
		// let mut items = vec![];
		// let mut offset = 0;
		// loop {
		// 	let page = self
		// 		.spotify
		// 		.artists(id)
		// 		.get_artist_albums(id, None, 50, offset, None)
		// 		.await?;
		//
		// 	for album in &mut page.data.items.iter() {
		// 		items.append(&mut self.full_album(&album.id).await?)
		// 	}
		//
		// 	// End
		// 	offset += page.data.items.len();
		// 	if page.data.total == offset {
		// 		return Ok(items);
		// 	}
		// }
		let mut albums: Vec<SimplifiedAlbum> = Vec::new();
		let stream = self
			.spotify
			.artist_albums(ArtistId::from_id(id).unwrap(), None, None);
		pin_mut!(stream);
		while let Some(item) = stream.try_next().await.unwrap() {
			albums.push(item);
		}

		let mut tracks: Vec<SimplifiedTrack> = Vec::new();
		for album in albums {
			let stream = self.spotify.album_track(album.id.unwrap(), None);
			pin_mut!(stream);
			while let Some(item) = stream.try_next().await.unwrap() {
				tracks.push(item);
			}
		}

		Ok(tracks)
	}
}

impl Clone for Spotify {
	fn clone(&self) -> Self {
		Self {
			session: self.session.clone(),
			spotify: ClientCredsSpotify::new(self.spotify.creds.clone()),
		}
	}
}

/// Basic debug implementation so can be used in other structs
impl fmt::Debug for Spotify {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "<Spotify Instance>")
	}
}

#[derive(Debug, Clone)]
pub enum SpotifyItem {
	Track(FullTrack),
	Album(FullAlbum),
	Playlist(FullPlaylist),
	Artist(FullArtist),
	/// Unimplemented
	Other(String),
}
