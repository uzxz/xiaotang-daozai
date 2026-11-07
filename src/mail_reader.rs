//! 邮件全文阅读窗口：完整文字、收件账号和网页链接。
use crate::app::{App,Cmd};
use crate::translate::View;
use eframe::egui::{self,Color32};

impl App {
    pub fn mail_reader_ui(&mut self,ctx:&egui::Context) {
        let Some(raw)=self.mail.reading.clone() else{return};
        let hidden=self.mail_code_hidden(&raw);
        let item=self.display_mail(&raw);
        let mut reveal=false;
        let focus=std::mem::take(&mut self.mail.reader_focus);
        let (mut close,mut link)=(false,None);
        let mut toggle=None;
        let (mut translate_req,mut translate_tgl,mut translate_redo)=(false,false,false);
        // 翻译入口与译文：状态在窗口外取好，窗口内只读取缓存。
        let view=self.translate_view(&item);
        let key=(item.account.clone(),item.uid);
        let translation=self.translate.cache.get(&key).and_then(|r|r.as_ref().ok()).cloned();
        let show_translation=view.ready() && translation.is_some() && self.translate.show.get(&key).copied().unwrap_or(true);
        let body_text=if show_translation{translation.as_deref().unwrap_or("")}else{item.summary.as_ref()};
        // 有原始 HTML 且不在遮蔽期、未显示译文时，正文按富文本块渲染。
        let has_html=item.html.is_some() && !hidden && !show_translation;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("daozai-mail-reader"),
            egui::ViewportBuilder::default().with_title("邮件全文 · 岛仔").with_inner_size([700.0,620.0]).with_min_inner_size([500.0,380.0]),|ctx,_|{
            if focus{ctx.send_viewport_cmd(egui::ViewportCommand::Focus);}
            close=ctx.input(|i|i.viewport().close_requested());
            if raw.code.is_some(){ctx.request_repaint_after(std::time::Duration::from_secs(1));}
            egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(25,25,27)).inner_margin(22)).show(ctx,|ui|{
                ui.spacing_mut().item_spacing.y=8.0;
                ui.add(egui::Label::new(egui::RichText::new(&item.subject).size(crate::theme::ui_size(20.0)).strong().color(Color32::WHITE)).wrap());
                egui::Grid::new("mail-reader-meta").spacing([12.0,5.0]).show(ui,|ui|{
                    for (label,text) in [("发件人",&item.from),("收件邮箱",&item.account),("时间",&item.time)]{
                        ui.label(egui::RichText::new(label).color(Color32::from_gray(160)));
                        ui.label(egui::RichText::new(text).color(Color32::from_gray(220)));ui.end_row();
                    }
                });
                if let Some(event)=&item.meeting {
                    ui.horizontal_wrapped(|ui|{
                        ui.label(format!("日程 · {} · {}",event.at,event.title));
                        if ui.button(if self.mail_event_ignored(event){"恢复日程"}else{"忽略日程"}).clicked(){toggle=Some(event.clone());}
                    });
                }
                // 翻译工具行：没配账号或验证码遮蔽期不显示入口。
                match view {
                    View::NoAccounts|View::Hidden=>{},
                    View::Idle=>{if ui.button("译成中文").on_hover_text("调用阿里云机器翻译，把正文译成中文；正文会发送到翻译服务").clicked(){translate_req=true;}},
                    View::Busy=>{ui.horizontal(|ui|{ui.spinner();ui.label("翻译中…");});},
                    View::Ready{shown}=>{ui.horizontal(|ui|{
                        if ui.button(if shown{"显示原文"}else{"显示译文"}).clicked(){translate_tgl=true;}
                        if ui.button("重新翻译").on_hover_text("重新请求翻译").clicked(){translate_redo=true;}
                    });},
                }
                if hidden {
                    ui.label("验证码已隐藏，不代表验证码失效；相关链接也暂时隐藏。");
                    reveal=ui.button("主动查看验证码与正文 15 秒").clicked();
                }
                ui.separator();
                ui.label(egui::RichText::new(if show_translation{"以下为机器译文，仅供参考"}else{"鼠标滚轮或拖动右侧滚动条查看全文"}).size(crate::theme::ui_size(11.5)).color(Color32::from_gray(160)));
                ui.style_mut().spacing.scroll.floating=false;
                egui::ScrollArea::vertical().id_salt(("mail-reader-body",&item.account,item.uid)).auto_shrink([false,false]).show(ui,|ui|{
                    if has_html {
                        // 需要时解析（缓存进 MailState），随后块缓存与纹理缓存分字段借用，避免逐帧克隆正文。
                        if self.mail_blocks(&item).is_some(){
                            if let Some(blocks)=self.mail.html_cache.as_ref().map(|(_,blocks)|blocks){
                                crate::mail_html::render(ui,blocks,&item,&mut self.mail.html_textures);
                            }
                        }
                    }else{
                        ui.add(egui::Label::new(egui::RichText::new(if body_text.is_empty(){"（这封邮件没有正文）"}else{body_text}).size(crate::theme::ui_size(14.0)).color(if show_translation{Color32::from_rgb(150,200,255)}else{Color32::from_gray(235)})).selectable(true).wrap());
                    }
                    if !show_translation && !item.links.is_empty(){
                        ui.add_space(12.0);ui.separator();ui.label(egui::RichText::new("邮件中的链接").strong());
                        for target in &item.links{
                            if ui.link(&target.label).on_hover_text(&target.url).clicked(){link=Some(target.url.clone());}
                        }
                    }
                });
            });
        });
        if translate_req{self.translate_request(&raw);ctx.request_repaint();}
        if translate_redo{self.translate_retry(&raw);ctx.request_repaint();}
        if translate_tgl{self.translate_toggle(&raw);ctx.request_repaint();}
        if reveal{self.reveal_mail_code(&raw);ctx.request_repaint();}
        if let Some(event)=toggle{self.toggle_mail_event(&event);}
        if close{self.mail.reading=None;}
        if let Some(url)=link{self.pending.push(Cmd::OpenUrl(url));ctx.request_repaint();}
    }
}
