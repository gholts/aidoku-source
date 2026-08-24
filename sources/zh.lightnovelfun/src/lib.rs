#![no_std]

use aidoku::{
	Chapter, ContentRating, DeepLinkHandler, DeepLinkResult, FilterValue, Manga, MangaPageResult,
	MangaStatus, Page, PageContent, Result, Source,
	alloc::{String, Vec, string::ToString, vec},
	imports::{
		html::{Element, Html, Kind},
		net::{Request, TimeUnit, set_rate_limit},
		std::{parse_date, send_partial_result},
	},
	prelude::*,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

const BASE_URL: &str = "https://www.lightnovel.fun";
const API_URL: &str = "https://www.lightnovel.fun/api/pc-proxy";

struct LightNovelFun;

impl Source for LightNovelFun {
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
		let query = query.unwrap_or_default();
		let data: BookList = if query.trim().is_empty() {
			post_api(
				"/api/bff/home-feed-v1",
				&BrowseRequest {
					page: page.max(1),
					page_size: 20,
					page_size_camel: 20,
					read_filter: "all",
					status_filter: "all",
					category_filter: "all",
				},
			)?
		} else {
			post_api(
				"/api/bff/apk-search-result-v1",
				&SearchRequest {
					q: &query,
					scope: "",
					source: "",
					primary_tag: "",
					channel_code: "",
					work_type: "",
					source_type: "",
					filters: Value::Object(serde_json::Map::new()),
					word_count_bucket: "",
					status_bucket: "",
					page: page.saturating_sub(1),
					page_size: 20,
					sort: "relevance",
					preset: "",
				},
			)?
		};
		Ok(MangaPageResult {
			entries: data.list.into_iter().map(book_to_manga).collect(),
			has_next_page: data.pagination.page < data.pagination.page_count,
		})
	}

	fn get_manga_update(
		&self,
		mut manga: Manga,
		needs_details: bool,
		needs_chapters: bool,
	) -> Result<Manga> {
		let book_id = manga.key.clone();
		if needs_details {
			let book: Book = post_api(
				"/api/new-content-read/get-book-detail",
				&BookRequest {
					book_id: &book_id,
					with_volumes: 0,
				},
			)?;
			manga.copy_from(book_to_manga(book));
			if needs_chapters {
				send_partial_result(&manga);
			}
		}
		if needs_chapters {
			manga.chapters = Some(fetch_chapters(&book_id)?);
		}
		Ok(manga)
	}

	fn get_page_list(&self, manga: Manga, chapter: Chapter) -> Result<Vec<Page>> {
		let data: ChapterDetail = post_api(
			"/api/new-content-read/get-chapter-detail",
			&ChapterRequest {
				book_id: &manga.key,
				chapter_id: &chapter.key,
			},
		)?;
		if value_truthy(&data.locked) {
			bail!("chapter is locked");
		}
		let html = if !data.body_snapshot.body_html.is_empty() {
			&data.body_snapshot.body_html
		} else {
			&data.render_preview.body_html
		};
		let mut text = html_to_markdown(html)?;
		if text.trim().is_empty() {
			text = if !data.body_snapshot.body_text.is_empty() {
				data.body_snapshot.body_text
			} else {
				data.render_preview.body_text
			};
		}
		if text.trim().is_empty() {
			bail!("chapter text not found");
		}
		Ok(vec![Page {
			content: PageContent::text(text),
			..Default::default()
		}])
	}
}

impl DeepLinkHandler for LightNovelFun {
	fn handle_deep_link(&self, url: String) -> Result<Option<DeepLinkResult>> {
		let path = url
			.split(['?', '#'])
			.next()
			.unwrap_or(&url)
			.rsplit("lightnovel.fun")
			.next()
			.unwrap_or("")
			.trim_matches('/');
		let mut parts = path.split('/');
		match (parts.next(), parts.next(), parts.next()) {
			(Some("book"), Some(book), _) if is_id(book) => {
				Ok(Some(DeepLinkResult::Manga { key: book.into() }))
			}
			(Some("reader"), Some(book), Some(chapter)) if is_id(book) && is_id(chapter) => {
				Ok(Some(DeepLinkResult::Chapter {
					manga_key: book.into(),
					key: chapter.into(),
				}))
			}
			_ => Ok(None),
		}
	}
}

fn is_id(value: &str) -> bool {
	!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

fn post_api<T: DeserializeOwned, B: Serialize>(path: &str, body: &B) -> Result<T> {
	let body = serde_json::to_string(body)?;
	let response = Request::post(format!("{API_URL}{path}"))?
		.header("Accept", "application/json")
		.header("Content-Type", "application/json")
		.header("Origin", BASE_URL)
		.header("Referer", BASE_URL)
		.body(body)
		.send()?;
	if !(200..300).contains(&response.status_code()) {
		bail!("LightNovel.fun HTTP {}", response.status_code());
	}
	let envelope: ApiResponse<T> = response.get_json_owned()?;
	if envelope.code != 0 {
		bail!(
			"LightNovel.fun API error {}: {}",
			envelope.code,
			envelope.message.unwrap_or_default()
		);
	}
	Ok(envelope.data)
}

fn fetch_chapters(book_id: &str) -> Result<Vec<Chapter>> {
	let mut volumes = Vec::new();
	let mut page = 1;
	loop {
		let data: VolumeList = post_api(
			"/api/new-content-read/get-book-volumes",
			&PageRequest {
				book_id,
				volume_id: None,
				page,
				page_size: 50,
			},
		)?;
		let page_count = data.pagination.page_count.max(1);
		volumes.extend(data.list);
		if page >= page_count {
			break;
		}
		page += 1;
	}

	let mut chapters = Vec::new();
	let mut ordinal = 0_f32;
	for (volume_index, volume) in volumes.into_iter().enumerate() {
		let volume_id = volume.volume_id.to_string();
		let mut chapter_page = 1;
		loop {
			let data: ChapterList = post_api(
				"/api/new-content-read/get-volume-chapters",
				&PageRequest {
					book_id,
					volume_id: Some(&volume_id),
					page: chapter_page,
					page_size: 50,
				},
			)?;
			let page_count = data.pagination.page_count.max(1);
			for item in data.list {
				ordinal += 1.0;
				if value_truthy(&item.locked) {
					continue;
				}
				let title = if volume.title.is_empty() {
					item.title.clone()
				} else {
					format!("{} — {}", volume.title, item.title)
				};
				chapters.push(Chapter {
					key: item.chapter_id.to_string(),
					title: Some(title),
					chapter_number: Some(ordinal),
					volume_number: Some(volume_index as f32 + 1.0),
					date_uploaded: parse_date(&item.published_at, "yyyy-MM-dd HH:mm:ss"),
					url: Some(format!("{BASE_URL}/reader/{book_id}/{}", item.chapter_id)),
					language: Some("zh".into()),
					..Default::default()
				});
			}
			if chapter_page >= page_count {
				break;
			}
			chapter_page += 1;
		}
	}
	chapters.reverse();
	Ok(chapters)
}

fn book_to_manga(book: Book) -> Manga {
	let tag_values = if book.visible_tags.is_empty() {
		&book.tags
	} else {
		&book.visible_tags
	};
	let tags = tag_values.iter().filter_map(tag_name).collect::<Vec<_>>();
	let description = if !book.summary.is_empty() {
		Some(book.summary)
	} else if !book.summary_short.is_empty() {
		Some(book.summary_short)
	} else {
		None
	};
	Manga {
		key: book.book_id.to_string(),
		title: book.title,
		cover: nonempty(book.cover_url),
		authors: nonempty(book.author_name).map(|author| vec![author]),
		description,
		url: Some(format!("{BASE_URL}/book/{}", book.book_id)),
		tags: (!tags.is_empty()).then_some(tags),
		status: if value_truthy(&book.is_completed)
			|| book
				.serial_status
				.as_deref()
				.is_some_and(|status| status.eq_ignore_ascii_case("completed"))
		{
			MangaStatus::Completed
		} else {
			MangaStatus::Ongoing
		},
		content_rating: ContentRating::Suggestive,
		..Default::default()
	}
}

fn nonempty(value: String) -> Option<String> {
	(!value.trim().is_empty()).then_some(value)
}

fn tag_name(value: &Value) -> Option<String> {
	if let Some(value) = value.as_str() {
		return (!value.is_empty()).then(|| value.to_string());
	}
	let object = value.as_object()?;
	["name", "tag_name", "label", "title"]
		.into_iter()
		.find_map(|key| object.get(key)?.as_str().map(ToString::to_string))
}

fn value_truthy(value: &Value) -> bool {
	value.as_bool().unwrap_or(false)
		|| value.as_i64().is_some_and(|n| n != 0)
		|| value
			.as_str()
			.is_some_and(|s| matches!(s, "1" | "true" | "locked" | "completed"))
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
	let document = Html::parse_fragment_with_url(fragment, BASE_URL)?;
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
				if let Some(text) = node.text() {
					output.push_str(&escape_markdown(&text));
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
	let tag = element.tag_name().unwrap_or_default();
	match tag.as_str() {
		"script" | "style" => {}
		"p" | "div" | "section" => {
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
		"h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
			let level = tag.as_bytes()[1] - b'0';
			for _ in 0..level {
				output.push('#');
			}
			output.push(' ');
			convert_children(element, output);
			output.push_str("\n\n");
		}
		_ => convert_children(element, output),
	}
}

#[derive(Deserialize)]
struct ApiResponse<T> {
	#[serde(default)]
	code: i32,
	#[serde(default, alias = "msg")]
	message: Option<String>,
	data: T,
}

#[derive(Default, Deserialize)]
struct Pagination {
	#[serde(default)]
	page: i32,
	#[serde(default)]
	page_count: i32,
}

#[derive(Default, Deserialize)]
struct BookList {
	#[serde(default)]
	list: Vec<Book>,
	#[serde(default)]
	pagination: Pagination,
}

#[derive(Default, Deserialize)]
struct Book {
	#[serde(default)]
	book_id: u64,
	#[serde(default)]
	title: String,
	#[serde(default)]
	author_name: String,
	#[serde(default)]
	cover_url: String,
	#[serde(default)]
	summary: String,
	#[serde(default)]
	summary_short: String,
	#[serde(default)]
	serial_status: Option<String>,
	#[serde(default)]
	is_completed: Value,
	#[serde(default)]
	tags: Vec<Value>,
	#[serde(default)]
	visible_tags: Vec<Value>,
}

#[derive(Default, Deserialize)]
struct VolumeList {
	#[serde(default)]
	list: Vec<Volume>,
	#[serde(default)]
	pagination: Pagination,
}

#[derive(Default, Deserialize)]
struct Volume {
	#[serde(default)]
	volume_id: u64,
	#[serde(default)]
	title: String,
}

#[derive(Default, Deserialize)]
struct ChapterList {
	#[serde(default)]
	list: Vec<ChapterItem>,
	#[serde(default)]
	pagination: Pagination,
}

#[derive(Default, Deserialize)]
struct ChapterItem {
	#[serde(default)]
	chapter_id: u64,
	#[serde(default)]
	title: String,
	#[serde(default)]
	published_at: String,
	#[serde(default)]
	locked: Value,
}

#[derive(Default, Deserialize)]
struct ChapterDetail {
	#[serde(default)]
	locked: Value,
	#[serde(default)]
	body_snapshot: ChapterBody,
	#[serde(default)]
	render_preview: ChapterBody,
}

#[derive(Default, Deserialize)]
struct ChapterBody {
	#[serde(default)]
	body_html: String,
	#[serde(default)]
	body_text: String,
}

#[derive(Serialize)]
struct SearchRequest<'a> {
	q: &'a str,
	scope: &'static str,
	source: &'static str,
	primary_tag: &'static str,
	channel_code: &'static str,
	work_type: &'static str,
	preset: &'a str,
	source_type: &'static str,
	filters: Value,
	word_count_bucket: &'static str,
	status_bucket: &'static str,
	page: i32,
	#[serde(rename = "pageSize")]
	page_size: i32,
	sort: &'a str,
}

#[derive(Serialize)]
struct BrowseRequest {
	page: i32,
	page_size: i32,
	#[serde(rename = "pageSize")]
	page_size_camel: i32,
	read_filter: &'static str,
	status_filter: &'static str,
	category_filter: &'static str,
}

#[derive(Serialize)]
struct BookRequest<'a> {
	book_id: &'a str,
	with_volumes: i32,
}

#[derive(Serialize)]
struct PageRequest<'a> {
	book_id: &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	volume_id: Option<&'a str>,
	page: i32,
	#[serde(rename = "pageSize")]
	page_size: i32,
}

#[derive(Serialize)]
struct ChapterRequest<'a> {
	book_id: &'a str,
	chapter_id: &'a str,
}

register_source!(LightNovelFun, DeepLinkHandler);
