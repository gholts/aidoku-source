#![no_std]

use aidoku::{
	Chapter, ContentRating, DeepLinkHandler, DeepLinkResult, FilterValue, Manga, MangaPageResult,
	MangaStatus, Page, PageContent, Result, Source,
	alloc::{String, Vec, string::ToString, vec},
	helpers::uri::encode_uri_component,
	imports::{
		html::{Document, Element, Html, Kind},
		js::WebView,
		net::{Request, TimeUnit, set_rate_limit},
		std::{send_partial_result, sleep},
	},
	prelude::*,
};
use serde::{Deserialize, de::DeserializeOwned};

const BASE_URL: &str = "https://tw.linovelib.com";
const API_URL: &str = "https://lnovel.animes.garden";
const SEARCH_URL: &str = "https://cse.google.com/cse?cx=649de34f5e63448cb&hl=en";
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

		search(query.trim(), page)
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
	let response = Request::get(format!("{API_URL}/api{path}"))?
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

fn search_url(query: &str, page: usize) -> String {
	let query = encode_uri_component(format!(
		"site:tw.linovelib.com/novel/ {query} more:pagemap:metatags-og_type:novel"
	));
	format!("{SEARCH_URL}&q={query}#gsc.tab=0&gsc.q={query}&gsc.page={page}")
}

fn search(query: &str, page: usize) -> Result<MangaPageResult> {
	let url = search_url(query, page);
	let view = WebView::new();
	view.load(Request::get(&url)?)?;
	let ready_html = format!(
		r#"(() => {{
		const results = document.querySelector('.gsc-results.gsc-webResult');
		if (!results || document.querySelector('.gsc-loading-resultsRoot')) return '';
		const current = results.querySelector('.gsc-cursor-current-page');
		const empty = results.querySelector('.gs-no-results-result');
		const entries = results.querySelector('.gsc-expansionArea .gsc-result');
		if (empty || (entries && (current ? current.textContent.trim() === '{page}' : {page} === 1))) return results.outerHTML;
		return '';
	}})()"#
	);
	// Loading can fail without signalling wait_for_load; keep the wait bounded.
	for _ in 0..30 {
		sleep(1);
		let html = view.eval(&ready_html).unwrap_or_default();
		if !html.is_empty() {
			return parse_search_results(&Html::parse_with_url(html.as_bytes(), &url)?, page);
		}
	}
	bail!(
		"Linovelib search did not load; open its website search to check verification, then retry"
	)
}

fn parse_search_results(html: &Document, page: usize) -> Result<MangaPageResult> {
	let mut entries = Vec::new();
	for result in html
		.select(".gsc-expansionArea .gsc-webResult.gsc-result")
		.into_iter()
		.flatten()
	{
		let Some(link) = result.select_first("a.gs-title[href]") else {
			continue;
		};
		let Some(url) = link.attr("href") else {
			continue;
		};
		let Some(key) = url
			.strip_prefix(BASE_URL)
			.or_else(|| url.strip_prefix("http://tw.linovelib.com"))
			.and_then(|path| path.strip_prefix("/novel/"))
			.and_then(|path| path.split(['/', '?', '#']).next())
			.map(|key| key.trim_end_matches(".html"))
			.filter(|key| is_id(key))
		else {
			continue;
		};
		if entries.iter().any(|entry: &Manga| entry.key == key) {
			continue;
		}
		let title = link.text().unwrap_or_default();
		if title.is_empty() {
			continue;
		}
		entries.push(Manga {
			key: key.into(),
			title,
			cover: result
				.select_first("img.gs-image")
				.and_then(|image| image.attr("abs:src")),
			description: result
				.select_first(".gs-snippet")
				.and_then(|snippet| snippet.text()),
			url: Some(format!("{BASE_URL}/novel/{key}.html")),
			content_rating: ContentRating::Suggestive,
			..Default::default()
		});
	}
	if entries.is_empty() && html.select_first(".gs-no-results-result").is_none() {
		bail!("Linovelib search returned an unexpected page");
	}
	Ok(MangaPageResult {
		entries,
		has_next_page: html
			.select(".gsc-cursor-page")
			.into_iter()
			.flatten()
			.filter_map(|element| element.text()?.trim().parse::<usize>().ok())
			.any(|number| number > page),
	})
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
	if !novel.done {
		bail!("Linovelib is still caching this book; try updating it again later");
	}
	let mut chapters = Vec::new();
	let mut ordinal = 0_f32;
	for (volume_index, volume) in novel.volumes.iter().enumerate() {
		let detail: ApiVolume = get_api(&format!("/bili/novel/{}/vol/{}", novel.nid, volume.vid))?;
		if !detail.done {
			bail!("Linovelib is still caching this volume; try updating it again later");
		}
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
	done: bool,
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
	done: bool,
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

#[cfg(test)]
mod tests {
	use super::*;
	use aidoku_test::aidoku_test;

	#[aidoku_test]
	fn incomplete_api_cache_cannot_replace_a_saved_catalog() {
		let novel = ApiNovel {
			nid: 5455,
			done: false,
			..Default::default()
		};
		assert!(fetch_chapters(&novel).is_err());
	}

	#[aidoku_test]
	fn searches_uncached_books_and_groups_chapter_links_by_book() {
		let html = Html::parse_with_url(br#"<div class="gsc-expansionArea">
			<div class="gsc-webResult gsc-result">
				<a class="gs-title" href="https://tw.linovelib.com/novel/5455.html">World Order</a>
				<img class="gs-image" src="https://images.example/cover.jpg">
				<div class="gs-snippet">Novel synopsis</div>
			</div>
			<div class="gsc-webResult gsc-result"><a class="gs-title" href="http://tw.linovelib.com/novel/5455.html">Duplicate</a></div>
			<div class="gsc-webResult gsc-result"><a class="gs-title" href="https://tw.linovelib.com/novel/5455/341358.html">Chapter</a></div>
			<div class="gsc-webResult gsc-result"><a class="gs-title" href="https://tw.linovelib.com/novel/5243/catalog">Another novel</a></div>
			<div class="gsc-webResult gsc-result"><a class="gs-title" href="https://tw.linovelib.com.evil.test/novel/1.html">Unrelated</a></div>
		</div><div class="gsc-cursor-page">1</div><div class="gsc-cursor-page">2</div>"#, SEARCH_URL).unwrap();
		let result = parse_search_results(&html, 1).unwrap();
		assert_eq!(result.entries.len(), 2);
		assert_eq!(result.entries[0].key, "5455");
		assert_eq!(result.entries[0].title, "World Order");
		assert_eq!(result.entries[1].key, "5243");
		assert_eq!(
			result.entries[1].url.as_deref(),
			Some("https://tw.linovelib.com/novel/5243.html")
		);
		assert_eq!(
			result.entries[0].description.as_deref(),
			Some("Novel synopsis")
		);
		assert!(result.has_next_page);
		assert!(!parse_search_results(&html, 2).unwrap().has_next_page);
	}

	#[aidoku_test]
	fn distinguishes_no_results_from_a_failed_search() {
		let empty = Html::parse_with_url(
			b"<div class='gs-no-results-result'>No results</div>",
			SEARCH_URL,
		)
		.unwrap();
		let result = parse_search_results(&empty, 1).unwrap();
		assert!(result.entries.is_empty());
		assert!(!result.has_next_page);
		let blocked =
			Html::parse_with_url(b"<html><title>Sorry...</title></html>", SEARCH_URL).unwrap();
		assert!(parse_search_results(&blocked, 1).is_err());
	}

	#[aidoku_test]
	fn search_query_cannot_change_url_parameters() {
		let url = search_url("A&B #日本", 2);
		assert!(url.contains("A%26B%20%23%E6%97%A5%E6%9C%AC"));
		assert_eq!(url.matches('#').count(), 1);
		assert!(url.ends_with("&gsc.page=2"));
	}
}
