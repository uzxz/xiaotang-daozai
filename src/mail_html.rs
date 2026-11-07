//! 邮件 HTML → 富文本块：标题、列表、加粗/斜体、引用、内嵌图片（cid）。
//! 只解析不执行；复杂嵌套表格线性化（单元格用「｜」连接）；远程图片默认不加载（隐私优先）。

use eframe::egui;
use scraper::Node;
use ego_tree::NodeRef;
use std::collections::HashMap;

const MAX_IMAGE_W: f32 = 480.0;
const MAX_IMAGE_H: f32 = 340.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// 链接文本：蓝色下划线；点击仍用正文下方的链接列表（不做逐段命中测试）。
    pub link: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading { level: u8, spans: Vec<Span> },
    ListItem { marker: String, depth: usize, spans: Vec<Span> },
    Quote { depth: usize, spans: Vec<Span> },
    Rule,
    /// cid 命中内嵌图片；空 cid 表示远程图片（默认不加载，渲染占位说明）。
    Image { cid: String },
}

// ---------------------------------------------------------------- 解析

struct Walk<'a> {
    blocks: &'a mut Vec<Block>,
    spans: Vec<Span>,
    bold: bool,
    italic: bool,
    underline: bool,
    link: bool,
    /// 列表栈：(是否有序, 下一个序号)；栈深就是缩进层级。
    list: Vec<(bool, usize)>,
    quote: usize,
}

impl<'a> Walk<'a> {
    fn push_span(&mut self, text: String) {
        if text.is_empty() { return; }
        match self.spans.last_mut() {
            // 相邻同格式片段合并，渲染的 LayoutJob 更紧凑。
            Some(last) if (last.bold, last.italic, last.underline, last.link) == (self.bold, self.italic, self.underline, self.link) => last.text.push_str(&text),
            _ => self.spans.push(Span { text, bold: self.bold, italic: self.italic, underline: self.underline, link: self.link }),
        }
    }

    /// 行内片段收口：按当前上下文（标题/列表/引用）落成一个块。
    fn flush(&mut self, heading: Option<u8>) {
        let spans = std::mem::take(&mut self.spans);
        if spans.iter().any(|s| !s.text.trim().is_empty() || s.text.contains('\n')) {
            let block = match heading {
                Some(level) => Block::Heading { level, spans },
                None if self.quote > 0 => Block::Quote { depth: self.quote, spans },
                None => match self.list.last() {
                    Some((ordered, index)) => {
                        let depth = self.list.len() - 1;
                        let marker = if *ordered { format!("{index}.") } else { "•".into() };
                        Block::ListItem { marker, depth, spans }
                    }
                    None => Block::Paragraph(spans),
                },
            };
            self.blocks.push(block);
        }
    }

    fn walk(&mut self, node: NodeRef<Node>) {
        match node.value() {
            Node::Text(text) => {
                let collapsed = collapse(&text.text);
                if !collapsed.is_empty() { self.push_span(collapsed); }
            }
            Node::Element(el) => {
                let name = el.name().to_ascii_lowercase();
                match name.as_str() {
                    // 彻底跳过：脚本、样式与不可见部分。
                    "script" | "style" | "head" | "title" | "meta" | "link" | "noscript" | "svg" | "o:p" => {},
                    "br" => self.push_span("\n".into()),
                    "hr" => { self.flush(None); self.blocks.push(Block::Rule); },
                    "img" => {
                        self.flush(None);
                        let src = el.attr("src").unwrap_or_default();
                        if let Some(cid) = src.strip_prefix("cid:") {
                            self.blocks.push(Block::Image { cid: cid.to_string() });
                        } else if src.starts_with("http") {
                            self.blocks.push(Block::Image { cid: String::new() });
                        }
                    },
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        self.flush(None);
                        let level = name.as_bytes()[1] - b'0';
                        let saved = self.bold;
                        self.bold = true;
                        for child in node.children() { self.walk(child); }
                        self.bold = saved;
                        self.flush(Some(level));
                    },
                    "ul" | "ol" => {
                        self.flush(None);
                        self.list.push((name == "ol", 1));
                        for child in node.children() { self.walk(child); }
                        self.list.pop();
                        self.flush(None);
                    },
                    "li" => {
                        self.flush(None);
                        for child in node.children() { self.walk(child); }
                        self.flush(None);
                        // 条目收口时推进当前层序号。
                        if let Some(last) = self.list.last_mut() { last.1 += 1; }
                    },
                    "blockquote" => {
                        self.flush(None);
                        self.quote += 1;
                        for child in node.children() { self.walk(child); }
                        self.quote -= 1;
                        self.flush(None);
                    },
                    "table" => self.walk_table(node),
                    // 常见块级容器：进出各收口一次，让子块独立成段。
                    "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "aside" | "nav" | "center" | "figure" | "figcaption" | "pre" | "form" => {
                        self.flush(None);
                        for child in node.children() { self.walk(child); }
                        self.flush(None);
                    },
                    // 其余一律当作行内容器（b/strong/i/em/u/a/span/font/label/tbody/tr/td…）。
                    _ => {
                        let saved = (self.bold, self.italic, self.underline, self.link);
                        match name.as_str() {
                            "b" | "strong" | "th" => self.bold = true,
                            "i" | "em" | "cite" | "var" => self.italic = true,
                            "u" => self.underline = true,
                            "a" => self.link = true,
                            _ => {},
                        }
                        for child in node.children() { self.walk(child); }
                        (self.bold, self.italic, self.underline, self.link) = saved;
                    },
                }
            },
            _ => {},
        }
    }

    /// 表格线性化：按文档顺序取所有 tr，每行单元格用「｜」连接成一段。
    fn walk_table(&mut self, node: NodeRef<Node>) {
        self.flush(None);
        for tr in node.descendants() {
            let Node::Element(el) = tr.value() else { continue };
            if !el.name().eq_ignore_ascii_case("tr") { continue; }
            let mut spans: Vec<Span> = Vec::new();
            for cell in tr.descendants() {
                let Node::Element(ce) = cell.value() else { continue };
                let cn = ce.name().to_ascii_lowercase();
                if cn != "td" && cn != "th" { continue; }
                if !spans.is_empty() {
                    spans.push(Span { text: "   ｜   ".into(), bold: false, italic: false, underline: false, link: false });
                }
                let mut sink = Vec::new();
                let mut sub = Walk { blocks: &mut sink, spans: Vec::new(), bold: cn == "th", italic: self.italic, underline: self.underline, link: self.link, list: Vec::new(), quote: 0 };
                for child in cell.children() { sub.walk(child); }
                spans.extend(sub.spans);
            }
            if spans.iter().any(|s| !s.text.trim().is_empty()) { self.blocks.push(Block::Paragraph(spans)); }
        }
        self.flush(None);
    }
}

/// HTML 空白折叠：连续空白变一个空格；纯空白节点返回空串。
fn collapse(raw: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() { space = true; }
        else {
            if space && !out.is_empty() { out.push(' '); }
            space = false;
            out.push(ch);
        }
    }
    if space && !out.is_empty() { out.push(' '); }
    // 非空内容且原文以空白开头时保留一个空格，维持行内元素之间的词间距。
    if !out.is_empty() && raw.chars().next().is_some_and(|c| c.is_whitespace()) { out.insert(0, ' '); }
    out
}

/// 解析 HTML 正文为渲染块；只保留 body 里的可见内容。
pub fn parse(html: &str) -> Vec<Block> {
    let document = scraper::Html::parse_document(html);
    let mut sink = Vec::new();
    let mut walk = Walk { blocks: &mut sink, spans: Vec::new(), bold: false, italic: false, underline: false, link: false, list: Vec::new(), quote: 0 };
    for child in document.root_element().children() { walk.walk(child); }
    walk.flush(None);
    sink
}

// ---------------------------------------------------------------- 渲染

fn job(ui: &egui::Ui, spans: &[Span], size: f32, color: egui::Color32, link: egui::Color32) -> egui::text::LayoutJob {
    let mut out = egui::text::LayoutJob::default();
    out.wrap.max_width = ui.available_width();
    for span in spans {
        let c = if span.link { link } else { color };
        out.append(&span.text, 0.0, egui::TextFormat {
            font_id: crate::theme::font(size, if span.bold { crate::theme::W::Semibold } else { crate::theme::W::Regular }),
            color: c,
            underline: if span.link || span.underline { egui::Stroke::new(1.0, c) } else { egui::Stroke::NONE },
            italics: span.italic,
            ..Default::default()
        });
    }
    out
}

fn render_image(ui: &mut egui::Ui, cid: &str, item: &crate::mail::Item, textures: &mut HashMap<String, egui::TextureHandle>) {
    let dim = egui::RichText::new(if cid.is_empty() { "［远程图片未加载］" } else { "［图片缺失］" }).size(crate::theme::ui_size(12.0)).color(egui::Color32::from_gray(140));
    if cid.is_empty() { ui.label(dim); return; }
    let Some((_, bytes)) = item.images.iter().find(|(id, _)| id.eq_ignore_ascii_case(cid)) else { ui.label(dim); return; };
    let key = format!("mail-img-{}-{}-{cid}", item.account, item.uid);
    let ctx = ui.ctx().clone();
    let handle = textures.get(&key).cloned().or_else(|| {
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let handle = ctx.load_texture(key.clone(), egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()), egui::TextureOptions::LINEAR);
        textures.insert(key.clone(), handle.clone());
        Some(handle)
    });
    if let Some(texture) = handle {
        let size = texture.size_vec2();
        let scale = ((ui.available_width().min(MAX_IMAGE_W) / size.x).min(MAX_IMAGE_H / size.y)).min(1.0).max(0.05);
        ui.add(egui::Image::from_texture(&texture).max_size(egui::vec2(size.x * scale, size.y * scale)));
    }
}

/// 把块渲染进全文窗口的滚动区；字体大小跟随界面字体缩放。
pub fn render(ui: &mut egui::Ui, blocks: &[Block], item: &crate::mail::Item, textures: &mut HashMap<String, egui::TextureHandle>) {
    let base = crate::theme::ui_size(14.0);
    let link = crate::theme::BLUE;
    let body = egui::Color32::from_gray(235);
    for (i, block) in blocks.iter().enumerate() {
        match block {
            Block::Rule => { ui.separator(); },
            Block::Image { cid } => render_image(ui, cid, item, textures),
            Block::Paragraph(spans) => { ui.add(egui::Label::new(job(ui, spans, base, body, link)).selectable(true)); },
            Block::Heading { level, spans } => {
                let size = match level { 1 => base * 1.5, 2 => base * 1.32, 3 => base * 1.18, 4 => base * 1.08, _ => base };
                ui.add(egui::Label::new(job(ui, spans, size, egui::Color32::WHITE, link)).selectable(true));
            },
            Block::Quote { depth, spans } => {
                let indent = (16.0 * *depth as f32).min(64.0);
                ui.indent(egui::Id::new(("mail-indent", i)), |ui| {
                    ui.add(egui::Label::new(job(ui, spans, base, egui::Color32::from_gray(185), link)).selectable(true));
                });
            },
            Block::ListItem { marker, depth, spans } => {
                let indent = (18.0 * *depth as f32).min(72.0);
                ui.indent(egui::Id::new(("mail-list", i)), |ui| {
                    let mut spans = spans.clone();
                    spans.insert(0, Span { text: format!("{marker}  "), bold: false, italic: false, underline: false, link: false });
                    ui.add(egui::Label::new(job(ui, &spans, base, body, link)).selectable(true));
                });
            },
        }
    }
}

impl crate::app::App {
    /// 全文窗口正文块：有 HTML 时解析成富文本块（按邮件缓存，换信即重建）；没有则 None 走纯文本。
    pub(crate) fn mail_blocks(&mut self, item: &crate::mail::Item) -> Option<Vec<Block>> {
        let html = item.html.as_ref()?;
        let key = (item.account.clone(), item.uid);
        if self.mail.html_cache.as_ref().map(|(k, _)| *k != key).unwrap_or(true) {
            let blocks = parse(&html);
            self.mail.html_cache = Some((key, blocks));
            self.mail.html_textures.clear();
        }
        self.mail.html_cache.as_ref().map(|(_, blocks)| blocks.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_text(block: &Block) -> String {
        match block {
            Block::Paragraph(spans) | Block::Heading { spans, .. } | Block::Quote { spans, .. } | Block::ListItem { spans, .. } => spans.iter().map(|s| s.text.as_str()).collect(),
            _ => String::new(),
        }
    }

    #[test]
    fn headings_bold_and_lists_render_as_blocks(){
        let blocks=parse("<h1>标题</h1><p>普通 <b>加粗</b> 和 <a href='https://example.com'>链接</a></p><ul><li>第一项</li><li>第二项</li></ul><ol><li>有序</li></ol>");
        assert!(matches!(blocks[0],Block::Heading{level:1,..}));
        assert_eq!(spans_text(&blocks[0]),"标题");
        let paragraph=&blocks[1];
        let Block::Paragraph(spans)=paragraph else {panic!("应为段落")};
        assert_eq!(spans.iter().map(|s|s.text.as_str()).collect::<String>(),"普通 加粗 和 链接");
        assert!(spans.iter().any(|s|s.bold && s.text=="加粗"),"加粗片段应带样式");
        assert!(spans.iter().any(|s|s.link && s.text=="链接"),"链接文本应为蓝色样式");
        let items:Vec<&Block>=blocks.iter().filter(|b|matches!(b,Block::ListItem{..})).collect();
        assert_eq!(items.len(),3,"两条无序与一条有序");
        assert!(matches!(items[0],Block::ListItem{marker,spans,..} if marker=="•" && spans.iter().map(|s|s.text.as_str()).collect::<String>()=="第一项"));
        assert!(matches!(items[2],Block::ListItem{marker,..} if marker=="1."));
    }

    #[test]
    fn tables_linearize_and_scripts_are_dropped(){
        let blocks=parse("<table><tr><th>名称</th><th>价格</th></tr><tr><td>套餐</td><td>99 元</td></tr></table><script>alert(1)</script><style>p{}</style>");
        assert_eq!(blocks.len(),2,"脚本与样式不能产生内容块");
        let row=spans_text(&blocks[0]);
        assert_eq!(row,"名称   ｜   价格");
        let Block::Paragraph(spans)=&blocks[0] else {panic!("应为段落")};
        assert!(spans.iter().take(1).all(|s|s.bold),"表头单元格应加粗");
        assert_eq!(spans_text(&blocks[1]),"套餐   ｜   99 元");
    }

    #[test]
    fn cid_images_become_blocks_and_remote_images_are_blocked(){
        let blocks=parse("<p>见下图</p><img src=\"cid:logo001\"><img src=\"https://img.example.com/px.gif\">");
        let images:Vec<&Block>=blocks.iter().filter(|b|matches!(b,Block::Image{..})).collect();
        assert_eq!(images.len(),2,"内嵌图与远程图都应成为图片块");
        assert!(matches!(images[0],Block::Image{cid} if cid=="logo001"));
        assert!(matches!(images[1],Block::Image{cid} if cid.is_empty()),"远程图片不加载");
    }

    #[test]
    fn blockquote_and_line_breaks_keep_structure(){
        let blocks=parse("<blockquote><p>引用的内容</p></blockquote><p>第一行<br>第二行</p>");
        let Block::Quote{depth,..}=blocks[0] else {panic!("应为引用块")};
        assert_eq!(depth,1);
        let Block::Paragraph(spans)=&blocks[1] else {panic!("应为段落")};
        assert!(spans.iter().any(|s|s.text.contains('\n')),"br 应转为换行");
    }
}
