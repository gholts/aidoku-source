#![no_std]

use aidoku::{
	Chapter, ContentRating, DeepLinkHandler, DeepLinkResult, FilterValue, Manga, MangaPageResult,
	MangaStatus, Page, PageContent, Result, Source,
	alloc::{String, Vec, string::ToString, vec},
	imports::{
		html::{Element, Html, Kind},
		net::{Request, TimeUnit, set_rate_limit},
		std::send_partial_result,
	},
	prelude::*,
};
use serde::{Deserialize, de::DeserializeOwned};

const BASE_URL: &str = "https://tw.linovelib.com";
const API_URL: &str = "https://lnovel.animes.garden";
const PAGE_SIZE: usize = 30;

struct TwLinovelib;

impl Source for TwLinovelib {
	fn new() -> Self {
		set_rate_limit(12, 10, TimeUnit::Seconds);
		Self
	}

	fn get_search_manga_list(
		&self,
		query: Option<String>,
		page: i32,
		_filters: Vec<FilterValue>,
	) -> Result<MangaPageResult> {
		let page = page.max(1) as usize;
		let query = query.unwrap_or_default();
		if query.trim().is_empty() {
			let data: TopData = get_api(&format!("/bili/top/lastupdate?page={page}"))?;
			let has_next_page = data.items.len() >= PAGE_SIZE;
			return Ok(MangaPageResult {
				entries: data.items.iter().map(top_item_to_manga).collect(),
				has_next_page,
			});
		}

		let needle = query.trim().to_lowercase();
		let matches = get_api::<Vec<ApiNovel>>("/bili/novels")?
			.into_iter()
			.filter(|novel| novel_matches(novel, &needle))
			.collect::<Vec<_>>();
		let start = (page - 1) * PAGE_SIZE;
		let has_next_page = matches.len() > start + PAGE_SIZE;
		Ok(MangaPageResult {
			entries: matches
				.iter()
				.skip(start)
				.take(PAGE_SIZE)
				.map(novel_to_manga)
				.collect(),
			has_next_page,
		})
	}

	fn get_manga_update(
		&self,
		mut manga: Manga,
		needs_details: bool,
		needs_chapters: bool,
	) -> Result<Manga> {
		let novel: ApiNovel = get_api(&format!("/bili/novel/{}", manga.key))?;
		if needs_details {
			manga.copy_from(novel_to_manga(&novel));
			if needs_chapters {
				send_partial_result(&manga);
			}
		}
		if needs_chapters {
			manga.chapters = Some(fetch_chapters(&novel)?);
		}
		Ok(manga)
	}

	fn get_page_list(&self, manga: Manga, chapter: Chapter) -> Result<Vec<Page>> {
		let data: ApiChapter = get_api(&format!(
			"/bili/novel/{}/chapter/{}",
			manga.key, chapter.key
		))?;
		let text = html_to_markdown(&data.content)?;
		if text.trim().is_empty() {
			bail!("chapter text not found");
		}
		Ok(vec![Page {
			content: PageContent::text(text),
			..Default::default()
		}])
	}
}

impl DeepLinkHandler for TwLinovelib {
	fn handle_deep_link(&self, url: String) -> Result<Option<DeepLinkResult>> {
		let path = url
			.split(['?', '#'])
			.next()
			.unwrap_or(&url)
			.rsplit("linovelib.com")
			.next()
			.unwrap_or("")
			.trim_matches('/');
		let mut parts = path.split('/');
		if parts.next() != Some("novel") {
			return Ok(None);
		}
		let Some(book) = parts.next() else {
			return Ok(None);
		};
		let book = book.trim_end_matches(".html");
		if !is_id(book) {
			return Ok(None);
		}
		if let Some(chapter) = parts.next() {
			let chapter = chapter
				.trim_end_matches(".html")
				.split('_')
				.next()
				.unwrap_or("");
			if is_id(chapter) {
				return Ok(Some(DeepLinkResult::Chapter {
					manga_key: book.into(),
					key: chapter.into(),
				}));
			}
		}
		Ok(Some(DeepLinkResult::Manga { key: book.into() }))
	}
}

fn get_api<T: DeserializeOwned + Default>(path: &str) -> Result<T> {
	let response = Request::get(format!("{API_URL}{path}"))?
		.header("Accept", "application/json")
		.send()?;
	if !(200..300).contains(&response.status_code()) {
		bail!("Linovelib API HTTP {}", response.status_code());
	}
	let envelope: ApiEnvelope<T> = response.get_json_owned()?;
	if !envelope.ok {
		bail!(
			"Linovelib API: {}",
			envelope.message.unwrap_or_else(|| "request failed".into())
		);
	}
	envelope
		.data
		.ok_or_else(|| error!("Linovelib API data missing"))
}

fn novel_matches(novel: &ApiNovel, needle: &str) -> bool {
	novel.name.to_lowercase().contains(needle)
		|| novel
			.authors
			.iter()
			.any(|author| author.name.to_lowercase().contains(needle))
		|| novel
			.labels
			.iter()
			.any(|label| label.to_lowercase().contains(needle))
}

fn novel_to_manga(novel: &ApiNovel) -> Manga {
	let authors = novel
		.authors
		.iter()
		.filter(|author| author.position.is_empty() || author.position == "author")
		.map(|author| author.name.clone())
		.collect::<Vec<_>>();
	Manga {
		key: novel.nid.to_string(),
		title: novel.name.clone(),
		cover: nonempty(novel.cover.clone()),
		authors: (!authors.is_empty()).then_some(authors),
		description: nonempty(clean_description(&novel.description)),
		url: Some(format!("{BASE_URL}/novel/{}.html", novel.nid)),
		tags: (!novel.labels.is_empty()).then_some(novel.labels.clone()),
		status: status_from_labels(&novel.labels),
		content_rating: ContentRating::Suggestive,
		..Default::default()
	}
}

fn top_item_to_manga(item: &TopItem) -> Manga {
	Manga {
		key: item.nid.to_string(),
		title: item.title.clone(),
		cover: nonempty(item.cover.clone()),
		authors: nonempty(item.author.clone()).map(|author| vec![author]),
		description: nonempty(item.description.clone()),
		url: Some(format!("{BASE_URL}/novel/{}.html", item.nid)),
		status: if item.status.contains("完结") || item.status.contains("完結") {
			MangaStatus::Completed
		} else {
			MangaStatus::Ongoing
		},
		content_rating: ContentRating::Suggestive,
		..Default::default()
	}
}

fn fetch_chapters(novel: &ApiNovel) -> Result<Vec<Chapter>> {
	let mut chapters = Vec::new();
	let mut ordinal = 0_f32;
	for (volume_index, volume) in novel.volumes.iter().enumerate() {
		let detail: ApiVolume = get_api(&format!("/bili/novel/{}/vol/{}", novel.nid, volume.vid))?;
		let volume_title = if detail.name.is_empty() {
			&volume.title
		} else {
			&detail.name
		};
		let volume_label = trim_volume_label(&novel.name, volume_title, volume_index + 1);
		for item in detail.chapters {
			ordinal += 1.0;
			chapters.push(Chapter {
				key: item.cid.to_string(),
				title: Some(if volume_label.is_empty() {
					item.title
				} else {
					format!("{volume_label} — {}", item.title)
				}),
				chapter_number: Some(ordinal),
				volume_number: Some(volume_index as f32 + 1.0),
				url: Some(format!("{BASE_URL}/novel/{}/{}.html", novel.nid, item.cid)),
				language: Some("zh".into()),
				..Default::default()
			});
		}
	}
	chapters.reverse();
	Ok(chapters)
}

fn trim_volume_label(novel_title: &str, volume_title: &str, volume_number: usize) -> String {
	let mut label = volume_title.trim();
	if let Some(rest) = label.strip_prefix(novel_title.trim()) {
		label = rest;
	} else {
		let wrapped_title = format!("《{}》", novel_title.trim());
		if let Some(rest) = label.strip_prefix(&wrapped_title) {
			label = rest;
		}
	}
	label = trim_label_separators(label);

	let number = volume_number.to_string();
	if let Some(rest) = label.strip_prefix(&number)
		&& rest.chars().next().is_none_or(is_label_separator)
	{
		label = trim_label_separators(rest);
	}
	label.to_string()
}

fn trim_label_separators(value: &str) -> &str {
	value.trim_start_matches(is_label_separator)
}

fn is_label_separator(character: char) -> bool {
	character.is_whitespace() || matches!(character, '-' | '—' | '–' | ':' | '：' | '·')
}

fn status_from_labels(labels: &[String]) -> MangaStatus {
	if labels
		.iter()
		.any(|label| label.contains("完结") || label.contains("完結"))
	{
		MangaStatus::Completed
	} else {
		MangaStatus::Ongoing
	}
}

fn clean_description(value: &str) -> String {
	value
		.replace("<br />", "\n")
		.replace("<br/>", "\n")
		.replace("<br>", "\n")
		.trim()
		.to_string()
}

fn nonempty(value: String) -> Option<String> {
	(!value.trim().is_empty()).then_some(value)
}

fn is_id(value: &str) -> bool {
	!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn escape_markdown(text: &str) -> String {
	text.replace('\\', "\\\\")
		.replace('*', "\\*")
		.replace('_', "\\_")
		.replace('[', "\\[")
		.replace(']', "\\]")
}

fn html_to_markdown(fragment: &str) -> Result<String> {
	if fragment.trim().is_empty() {
		return Ok(String::new());
	}
	let document = Html::parse_fragment_with_url(fragment, API_URL)?;
	let Some(body) = document.select_first("body") else {
		return Ok(String::new());
	};
	let mut output = String::new();
	convert_children(&body, &mut output);
	Ok(output.trim().to_string())
}

fn convert_children(element: &Element, output: &mut String) {
	for node in element.child_nodes() {
		match node.kind() {
			Kind::TextNode => {
				if let Some(value) = node.text() {
					output.push_str(&escape_markdown(&value));
				}
			}
			Kind::Element => {
				if let Ok(element) = Element::try_from(node) {
					convert_tag(&element, output);
				}
			}
			_ => {}
		}
	}
}

fn convert_tag(element: &Element, output: &mut String) {
	match element.tag_name().unwrap_or_default().as_str() {
		"script" | "style" => {}
		"p" | "div" | "section" | "center" | "blockquote" => {
			convert_children(element, output);
			output.push_str("\n\n");
		}
		"br" => output.push_str("  \n"),
		"hr" => output.push_str("\n\n---\n\n"),
		"strong" | "b" => {
			output.push_str("**");
			convert_children(element, output);
			output.push_str("**");
		}
		"em" | "i" => {
			output.push('*');
			convert_children(element, output);
			output.push('*');
		}
		"img" => {
			if let Some(src) = element.attr("abs:src").or_else(|| element.attr("src"))
				&& !src.is_empty()
			{
				output.push_str(&format!("\n\n![]({src})\n\n"));
			}
		}
		_ => convert_children(element, output),
	}
}

#[derive(Deserialize)]
struct ApiEnvelope<T> {
	#[serde(default)]
	ok: bool,
	#[serde(default)]
	data: Option<T>,
	#[serde(default)]
	message: Option<String>,
}

#[derive(Default, Deserialize)]
struct TopData {
	#[serde(default)]
	items: Vec<TopItem>,
}

#[derive(Default, Deserialize)]
struct TopItem {
	#[serde(default)]
	nid: u64,
	#[serde(default)]
	title: String,
	#[serde(default)]
	cover: String,
	#[serde(default)]
	author: String,
	#[serde(default)]
	status: String,
	#[serde(default)]
	description: String,
}

#[derive(Default, Deserialize)]
struct ApiNovel {
	#[serde(default)]
	nid: u64,
	#[serde(default)]
	name: String,
	#[serde(default)]
	authors: Vec<ApiAuthor>,
	#[serde(default)]
	description: String,
	#[serde(default)]
	cover: String,
	#[serde(default)]
	labels: Vec<String>,
	#[serde(default)]
	volumes: Vec<ApiVolumeRef>,
}

#[derive(Default, Deserialize)]
struct ApiAuthor {
	#[serde(default)]
	name: String,
	#[serde(default)]
	position: String,
}

#[derive(Default, Deserialize)]
struct ApiVolumeRef {
	#[serde(default)]
	vid: u64,
	#[serde(default)]
	title: String,
}

#[derive(Default, Deserialize)]
struct ApiVolume {
	#[serde(default)]
	name: String,
	#[serde(default)]
	chapters: Vec<ApiChapterRef>,
}

#[derive(Default, Deserialize)]
struct ApiChapterRef {
	#[serde(default)]
	cid: u64,
	#[serde(default)]
	title: String,
}

#[derive(Default, Deserialize)]
struct ApiChapter {
	#[serde(default)]
	content: String,
}

register_source!(TwLinovelib, DeepLinkHandler);
