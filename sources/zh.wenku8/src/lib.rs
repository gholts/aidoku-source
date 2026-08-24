#![no_std]

use aidoku::{
	Chapter, ContentRating, DeepLinkHandler, DeepLinkResult, FilterValue, ImageRequestProvider,
	Manga, MangaPageResult, MangaStatus, Page, PageContent, Result, Source,
	alloc::{String, Vec, string::ToString, vec},
	imports::{
		html::{Document, Element, Html, Kind},
		net::{Request, TimeUnit, set_rate_limit},
		std::send_partial_result,
	},
	prelude::*,
};
use encoding_rs::GBK;

const BASE_URL: &str = "https://www.wenku8.net";
const USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";

struct Wenku8;

impl Source for Wenku8 {
	fn new() -> Self {
		set_rate_limit(10, 10, TimeUnit::Seconds);
		Self
	}

	fn get_search_manga_list(
		&self,
		query: Option<String>,
		page: i32,
		_filters: Vec<FilterValue>,
	) -> Result<MangaPageResult> {
		let page = page.max(1);
		let html = if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
			request_html(&format!(
				"{BASE_URL}/modules/article/search.php?searchtype=articlename&searchkey={}&page={page}",
				percent_encode_gbk(&query)
			))?
		} else {
			request_html(&format!(
				"{BASE_URL}/modules/article/toplist.php?sort=lastupdate&page={page}"
			))?
		};

		let mut entries = parse_grid(&html);
		if entries.is_empty()
			&& let Some(key) = detail_book_key(&html)
			&& let Some(entry) = parse_detail(&html, &key)
		{
			entries.push(entry);
		}
		let next = page + 1;
		Ok(MangaPageResult {
			entries,
			has_next_page: html
				.select_first(format!("#pagelink a[href*='page={next}']"))
				.is_some(),
		})
	}

	fn get_manga_update(
		&self,
		mut manga: Manga,
		needs_details: bool,
		needs_chapters: bool,
	) -> Result<Manga> {
		let detail_url = format!("{BASE_URL}/book/{}.htm", manga.key);
		let detail = request_html(&detail_url)?;
		if needs_details {
			let details = parse_detail(&detail, &manga.key)
				.ok_or_else(|| error!("novel details not found"))?;
			manga.copy_from(details);
			if needs_chapters {
				send_partial_result(&manga);
			}
		}
		if needs_chapters {
			let novel_id = manga
				.key
				.parse::<u32>()
				.map_err(|_| error!("invalid Wenku8 novel ID"))?;
			let catalog_url = format!("{BASE_URL}/modules/article/reader.php?aid={novel_id}");
			let chapters = parse_chapters(&request_html(&catalog_url)?, &catalog_url);
			if chapters.is_empty() {
				bail!("Wenku8 catalog returned no chapters");
			}
			manga.chapters = Some(chapters);
		}
		Ok(manga)
	}

	fn get_page_list(&self, _manga: Manga, chapter: Chapter) -> Result<Vec<Page>> {
		let url = if chapter.key.starts_with("http") {
			chapter.key
		} else {
			format!("{BASE_URL}{}", chapter.key)
		};
		let html = request_html(&url)?;
		for selector in ["#contentdp", "#title", "#footlink", "script", "style"] {
			if let Some(elements) = html.select(selector) {
				elements.for_each(Element::remove);
			}
		}
		let container = html
			.select_first("#content")
			.ok_or_else(|| error!("chapter text not found"))?;
		let mut output = String::new();
		convert_children(&container, &mut output);
		for notice in [
			"本文来自 轻小说文库(http://www.wenku8.com)",
			"台版 转自 轻之国度",
			"最新最全的日本动漫轻小说 轻小说文库(http://www.wenku8.com) 为你一网打尽！",
			"更多精彩热门日本轻小说、动漫小说，轻小说文库(http://www.wenku8.com) 为你一网打尽！",
		] {
			output = output.replace(notice, "");
		}
		let output = output.trim().to_string();
		if output.is_empty() || output == "null" {
			bail!("chapter text not available on desktop site");
		}
		Ok(vec![Page {
			content: PageContent::text(output),
			..Default::default()
		}])
	}
}

impl ImageRequestProvider for Wenku8 {
	fn get_image_request(
		&self,
		url: String,
		_context: Option<aidoku::PageContext>,
	) -> Result<Request> {
		Ok(Request::get(url)?
			.header("Cookie", "jieqiUserCharset=utf-8")
			.header("Referer", BASE_URL)
			.header("User-Agent", USER_AGENT))
	}
}

impl DeepLinkHandler for Wenku8 {
	fn handle_deep_link(&self, url: String) -> Result<Option<DeepLinkResult>> {
		let path = url
			.split(['?', '#'])
			.next()
			.unwrap_or(&url)
			.rsplit("wenku8.net")
			.next()
			.unwrap_or("");
		if let Some(key) = path
			.strip_prefix("/book/")
			.and_then(|key| key.strip_suffix(".htm"))
			&& is_id(key)
		{
			return Ok(Some(DeepLinkResult::Manga { key: key.into() }));
		}
		let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
		if segments.len() == 4
			&& segments[0] == "novel"
			&& is_id(segments[2])
			&& segments[3].ends_with(".htm")
		{
			let key = if path.starts_with('/') {
				path.into()
			} else {
				format!("/{path}")
			};
			return Ok(Some(DeepLinkResult::Chapter {
				manga_key: segments[2].into(),
				key,
			}));
		}
		Ok(None)
	}
}

fn configured_request(request: Request) -> Request {
	request
		.header("Accept", "text/html,application/xhtml+xml")
		.header("Accept-Language", "zh-TW,zh;q=0.9")
		.header("Cookie", "jieqiUserCharset=utf-8")
		.header("Referer", BASE_URL)
		.header("User-Agent", USER_AGENT)
}

fn request_html(url: &str) -> Result<Document> {
	response_html(configured_request(Request::get(url)?))
}

fn response_html(request: Request) -> Result<Document> {
	let response = request.send()?;
	if response.status_code() == 403
		|| response
			.get_header("cf-mitigated")
			.is_some_and(|value| value == "challenge")
	{
		bail!("Cloudflare blocked the Wenku8 request");
	}
	if !(200..400).contains(&response.status_code()) {
		bail!("Wenku8 HTTP {}", response.status_code());
	}
	let url = response.get_url().unwrap_or_else(|| BASE_URL.into());
	let data = response.get_data()?;
	let html = parse_html_data(&data, &url)?;
	if is_cloudflare_challenge(&html) {
		bail!("Cloudflare blocked the Wenku8 request");
	}
	Ok(html)
}

fn parse_html_data(data: &[u8], url: &str) -> Result<Document> {
	let html = if let Ok(text) = core::str::from_utf8(data) {
		Html::parse_with_url(text.as_bytes(), url)?
	} else {
		let (text, _, _) = GBK.decode(data);
		Html::parse_with_url(text.as_bytes(), url)?
	};
	Ok(html)
}

fn is_cloudflare_challenge(html: &Document) -> bool {
	html.select_first("#challenge-running, .cf-challenge")
		.is_some()
		|| html
			.select_first("title")
			.and_then(|title| title.text())
			.is_some_and(|title| title.contains("Just a moment"))
}

fn parse_grid(html: &Document) -> Vec<Manga> {
	let mut entries = Vec::new();
	for item in html.select("table.grid td > div").into_iter().flatten() {
		let Some(link) = item.select_first("a[href*='/book/']") else {
			continue;
		};
		let Some(href) = link.attr("abs:href") else {
			continue;
		};
		let Some(key) = book_key(&href) else {
			continue;
		};
		if entries.iter().any(|entry: &Manga| entry.key == key) {
			continue;
		}
		let title_link = item.select_first("a[title]").unwrap_or(link);
		let title = title_link
			.attr("title")
			.or_else(|| title_link.text())
			.unwrap_or_default();
		if title.is_empty() {
			continue;
		}
		let item_text = item.text().unwrap_or_default();
		entries.push(Manga {
			key,
			title,
			cover: item
				.select_first("img")
				.and_then(|image| image.attr("abs:src")),
			authors: extract_between(&item_text, "作者:", "/").map(|author| vec![author]),
			url: Some(href),
			content_rating: ContentRating::Suggestive,
			..Default::default()
		});
	}
	entries
}

fn parse_detail(html: &Document, key: &str) -> Option<Manga> {
	let title = html.select_first("#content span b")?.text()?;
	if title.is_empty() {
		return None;
	}
	let status_text = field_value(html, "文章状态").unwrap_or_default();
	let tags = html
		.select_first("#content b:contains(作品Tags)")
		.and_then(|element| element.text())
		.and_then(|value| {
			value
				.split_once('：')
				.or_else(|| value.split_once(':'))
				.map(|(_, value)| value.to_string())
		})
		.map(|value| {
			value
				.split([' ', '/', '、', ','])
				.filter(|tag| !tag.trim().is_empty())
				.map(|tag| tag.trim().to_string())
				.collect::<Vec<_>>()
		})
		.unwrap_or_default();
	Some(Manga {
		key: key.into(),
		title,
		cover: html
			.select_first("#content img")
			.and_then(|image| image.attr("abs:src"))
			.map(|url| url.replace("http://", "https://")),
		authors: field_value(html, "小说作者").map(|author| vec![author]),
		description: html
			.select("#content span")
			.into_iter()
			.flatten()
			.filter_map(|element| element.text())
			.find(|text| text.len() > 80),
		url: Some(format!("{BASE_URL}/book/{key}.htm")),
		tags: (!tags.is_empty()).then_some(tags),
		status: if status_text.contains("已完") || status_text.contains("完結") {
			MangaStatus::Completed
		} else {
			MangaStatus::Ongoing
		},
		content_rating: ContentRating::Suggestive,
		..Default::default()
	})
}

fn field_value(html: &Document, label: &str) -> Option<String> {
	let value = html
		.select_first(format!("#content td:contains({label})"))?
		.text()?;
	let index = value.find(['：', ':'])?;
	let separator = value[index..].chars().next()?;
	Some(value[index + separator.len_utf8()..].trim().to_string())
}

fn extract_between(text: &str, start: &str, end: &str) -> Option<String> {
	let value = text.split_once(start)?.1;
	Some(
		value
			.split_once(end)
			.map_or(value, |(value, _)| value)
			.trim()
			.to_string(),
	)
}

fn parse_chapters(html: &Document, catalog_url: &str) -> Vec<Chapter> {
	let mut volume = String::new();
	let mut ordinal = 0_f32;
	let mut chapters = Vec::new();
	for cell in html.select("td").into_iter().flatten() {
		let class = cell.attr("class").unwrap_or_default();
		if class.split_whitespace().any(|value| value == "vcss") {
			volume = cell.text().unwrap_or_default().trim().to_string();
			continue;
		}
		if !class.split_whitespace().any(|value| value == "ccss") {
			continue;
		}
		let Some(link) = cell.select_first("a") else {
			continue;
		};
		let Some(href) = link.attr("href").or_else(|| link.attr("abs:href")) else {
			continue;
		};
		let url = resolve_url(catalog_url, &href);
		if !url.contains("reader.php?") || !url.contains("cid=") {
			continue;
		}
		let chapter_title = link.text().unwrap_or_default().trim().to_string();
		if chapter_title.is_empty() {
			continue;
		}
		ordinal += 1.0;
		chapters.push(Chapter {
			key: url.clone(),
			title: Some(if volume.is_empty() {
				chapter_title
			} else {
				format!("{volume} — {chapter_title}")
			}),
			chapter_number: Some(ordinal),
			url: Some(url),
			language: Some("zh".into()),
			..Default::default()
		});
	}
	chapters.reverse();
	chapters
}

fn resolve_url(base: &str, href: &str) -> String {
	if href.starts_with("http://") || href.starts_with("https://") {
		return href.to_string();
	}
	if href.starts_with('/') {
		return format!("{BASE_URL}{href}");
	}
	let directory = base
		.rsplit_once('/')
		.map_or(base, |(directory, _)| directory);
	format!("{directory}/{href}")
}

fn detail_book_key(html: &Document) -> Option<String> {
	let href = html
		.select_first("a[href*='uservote.php?id=']")?
		.attr("href")?;
	let id = href.split("id=").nth(1)?.split('&').next()?;
	is_id(id).then(|| id.to_string())
}

fn book_key(url: &str) -> Option<String> {
	let key = url
		.split("/book/")
		.nth(1)?
		.split(['?', '#'])
		.next()?
		.trim_end_matches(".htm");
	is_id(key).then(|| key.to_string())
}

fn is_id(value: &str) -> bool {
	!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn percent_encode_gbk(value: &str) -> String {
	let (bytes, _, _) = GBK.encode(value);
	let mut output = String::new();
	const HEX: &[u8; 16] = b"0123456789ABCDEF";
	for byte in bytes.iter().copied() {
		if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
			output.push(byte as char);
		} else {
			output.push('%');
			output.push(HEX[(byte >> 4) as usize] as char);
			output.push(HEX[(byte & 0x0f) as usize] as char);
		}
	}
	output
}

fn escape_markdown(text: &str) -> String {
	text.replace('\\', "\\\\")
		.replace('*', "\\*")
		.replace('_', "\\_")
		.replace('[', "\\[")
		.replace(']', "\\]")
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
		"p" | "div" | "section" | "center" => {
			convert_children(element, output);
			output.push_str("\n\n");
		}
		"br" => output.push_str("  \n"),
		"hr" => output.push_str("\n\n---\n\n"),
		"img" => {
			if let Some(src) = element.attr("abs:src").filter(|src| !src.is_empty()) {
				output.push_str(&format!(
					"\n\n![]({})\n\n",
					src.replace("http://", "https://")
				));
			}
		}
		_ => convert_children(element, output),
	}
}

register_source!(Wenku8, DeepLinkHandler, ImageRequestProvider);

#[cfg(test)]
mod tests {
	use super::*;
	use aidoku_test::aidoku_test;

	#[aidoku_test]
	fn parses_gbk_reader_catalog() {
		let fixture = r#"<table>
            <tr><td class="vcss" colspan="4">第一卷</td></tr>
            <tr><td class="ccss"><a href="https://www.wenku8.net/modules/article/reader.php?aid=1&amp;cid=2">第一章</a></td></tr>
            <tr><td class="ccss"></td></tr>
        </table>"#;
		let (data, _, _) = GBK.encode(fixture);
		let url = "https://www.wenku8.net/modules/article/reader.php?aid=1";
		let html = parse_html_data(&data, url).unwrap();
		let chapters = parse_chapters(&html, url);

		assert_eq!(chapters.len(), 1);
		assert_eq!(chapters[0].title.as_deref(), Some("第一卷 — 第一章"));
		assert_eq!(
			chapters[0].key,
			"https://www.wenku8.net/modules/article/reader.php?aid=1&cid=2"
		);
	}
}
