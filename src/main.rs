#[macro_use]
extern crate log;

mod converter;
mod downloader;
mod error;
mod settings;
mod spotify;
mod tag;

use async_std::task;
use colored::Colorize;
use downloader::{DownloadState, Downloader};
use settings::Settings;
use spotify::Spotify;
use std::env;
use std::time::{Duration, Instant};

#[cfg(not(windows))]
#[tokio::main]
async fn main() {
	start().await;
}

#[cfg(windows)]
#[tokio::main]
async fn main() {
	use colored::control;

	//backwards compatibility.
	if control::set_virtual_terminal(true).is_ok() {};
	start().await;
}

async fn start() {
	env_logger::init();

	let settings = match Settings::load().await {
		Ok(settings) => {
			println!(
				"{} {}.",
				"Settings successfully loaded.\nContinuing with spotify account:".green(),
				settings.username
			);
			settings
		}
		Err(e) => {
			println!(
				"{} {}...",
				"Settings could not be loaded, because of the following error:".red(),
				e
			);
			let default_settings = Settings::new("username", "password", "client_id", "secret");
			match default_settings.save().await {
				Ok(_) => {
					println!(
						"{}",
						"..but default settings have been created successfully. Edit them and run the program again.".green()
					);
				}
				Err(e) => {
					println!(
						"{} {}",
						"..and default settings could not be written:".red(),
						e
					);
				}
			};
			return;
		}
	};

	let args: Vec<String> = env::args().collect();
	if args.len() <= 1 {
		println!(
			"Usage:\n{} <search_term> | <track_url> | <album_url> | <playlist_url> | <artist_url>",
			args[0]
		);
		return;
	}

	let spotify = match Spotify::new(
		&settings.username,
		&settings.password,
		&settings.client_id,
		&settings.client_secret,
	)
	.await
	{
		Ok(spotify) => {
			println!("{}", "Login succeeded.".green());
			spotify
		}
		Err(e) => {
			println!(
				"{} {}",
				"Login failed, possibly due to invalid credentials or settings:".red(),
				e
			);
			return;
		}
	};

	let input = args[1..].join(" ");

	let max_requests_per_min = 60.0;
	let timeout:u64 = ((((1.0/(max_requests_per_min/60.0)) * 1000.0) * (1.0)) as f32) as u64;
	println!("timeout set to: {:?}", timeout);

	let downloader = Downloader::new(settings.downloader, spotify);
	match downloader.handle_input(&input).await {
		Ok(search_results) => {
			if let Some(search_results) = search_results {
				print!("{esc}[2J{esc}[1;1H", esc = 27 as char);

				for (i, track) in search_results.iter().enumerate() {
					println!("{}: {} - {}", i + 1, track.author, track.title);
				}
				println!("{}", "Select the track (default: 1): ".green());

				let mut selection;
				loop {
					let mut input = String::new();
					std::io::stdin()
						.read_line(&mut input)
						.expect("Failed to read line");

					selection = input.trim().parse::<usize>().unwrap_or(1) - 1;

					if selection < search_results.len() {
						break;
					}
					println!("{}", "Invalid selection. Try again or quit (CTRL+C):".red());
				}

				let track = &search_results[selection];

				if let Err(e) = downloader
					.add_uri(&format!("spotify:track:{}", track.track_id))
					.await
				{
					error!(
						"{}",
						format!(
							"{}: {}",
							"Track could not be added to download queue.".red(),
							e
						)
					);
					return;
				}
			}

			let refresh = Duration::from_secs(settings.refresh_ui_seconds);
			let now = Instant::now();
			let mut time_elapsed: u64;

			// Number of songs to download
			let total_down = downloader.get_downloads().await.len();
			// Number of songs downloaded
			let mut num_down = 0;

			'outer: loop {
				print!("{esc}[2J{esc}[1;1H", esc = 27 as char);
				let mut exit_flag: i8 = 1;

				for download in downloader.get_downloads().await {
					let state = download.state;
					
					// Update the number of downloaded songs
					num_down = total_down - downloader.get_downloads().await.len();
					
					let progress = if state != DownloadState::Done {
						match state {
							DownloadState::Downloading(r, t) => {
								exit_flag &= 0;
								let p = r as f32 / t as f32 * 100.0;
								if p > 100.0 {
									"100%".to_string()
								} else {
									format!("{}%", p as i8)
								}
							}
							DownloadState::Post => {
								exit_flag &= 0;
								"Postprocessing... ".to_string()
							}
							DownloadState::None | DownloadState::Lock => {
								exit_flag &= 0;
								"Preparing... ".to_string()
							}
							DownloadState::Error(e) => {
								format!("{} ", e)
							}
							DownloadState::Done => {
								"Impossible state".to_string()
							},
						}
					} else {
						// num_down = num_down + 1;
						"Done.".to_string()
					};

					println!("{:<19}| {}", progress, download.title);
				}
				time_elapsed = now.elapsed().as_secs();
				if exit_flag == 1 {
					break 'outer;
				}

				println!("\nElapsed second(s): {}", time_elapsed);
				println!("Downloaded {} out of {}", num_down, total_down);
				task::sleep(refresh).await
			}
			// Update the number of downloaded songs one last time
			num_down = total_down - downloader.get_downloads().await.len();
			println!("Finished download(s) in {} second(s).", time_elapsed);
			println!("Downloaded {} out of {}", num_down, total_down);
		}
		Err(e) => {
			error!("{} {}", "Handling input failed:".red(), e)
		}
	}
}
