//! 邮件翻译：阿里云通用版机器翻译（TranslateGeneral）。
//! AccessKeyId 配置保存在 translate.json，Secret 只进 Windows 凭据管理器；
//! 多账号轮询（round-robin），单个账号失败自动切换下一个。

use serde_json::{json, Value};
use std::{collections::{HashMap,HashSet}, fs, io::Read as _, sync::atomic::{AtomicUsize, Ordering}};

pub const SERVICE: &str = "Daozai.AliMT";
const ENDPOINT: &str = "https://mt.aliyuncs.com/";
/// 单次请求的正文字节上限（阿里云限制 5000 字节，按段落切分后留余量）。
const CHUNK_BYTES: usize = 3000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {pub id:String, pub label:String, pub ak_id:String, pub enabled:bool}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {pub accounts:Vec<Account>}
impl Config {
    pub fn load()->Self {
        let Ok(raw)=fs::read(crate::model::config_dir().join("translate.json")) else {return Self::default();};
        serde_json::from_slice::<Value>(&raw).ok().map(|v|Self{
            accounts:v["accounts"].as_array().into_iter().flatten().filter_map(|a|Some(Account{
                id:a["id"].as_str()?.into(),label:a["label"].as_str().unwrap_or_default().into(),ak_id:a["ak_id"].as_str().unwrap_or_default().into(),enabled:a["enabled"].as_bool().unwrap_or(true)
            })).collect(),
        }).unwrap_or_default()
    }
    fn json(&self)->Value{json!({"accounts":self.accounts.iter().map(|a|json!({"id":a.id,"label":a.label,"ak_id":a.ak_id,"enabled":a.enabled})).collect::<Vec<_>>()})}
    pub fn save(&self){
        let dir=crate::model::config_dir();let _=fs::create_dir_all(&dir);
        if let Ok(raw)=serde_json::to_vec_pretty(&self.json()){let _=crate::model::atomic_write(&dir.join("translate.json"),&raw);}
    }
}

// ---------------------------------------------------------------- 凭据

#[cfg(windows)]
pub mod secrets {
    /// AccessKeySecret 只进 Windows 凭据管理器；键是账号 id。
    pub fn save(id: &str, secret: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(super::SERVICE, id).map_err(|e| format!("凭据管理器不可用：{e}"))?;
        entry.set_password(secret).map_err(|e| format!("保存 AccessKey 失败：{e}"))
    }
    pub fn load(id: &str) -> Result<String, String> {
        let entry = keyring::Entry::new(super::SERVICE, id).map_err(|e| format!("凭据管理器不可用：{e}"))?;
        entry.get_password().map_err(|_| "没有保存 AccessKeySecret".to_string())
    }
    pub fn delete(id: &str) {
        if let Ok(entry) = keyring::Entry::new(super::SERVICE, id) { let _ = entry.delete_credential(); }
    }
}
#[cfg(not(windows))]
pub mod secrets {
    pub fn save(_id: &str, _secret: &str) -> Result<(), String> { Err("仅 Windows 支持凭据管理器".into()) }
    pub fn load(_id: &str) -> Result<String, String> { Err("仅 Windows 支持凭据管理器".into()) }
    pub fn delete(_id: &str) {}
}

// ---------------------------------------------------------------- 阿里云 RPC 签名（V1）

/// RFC3986 百分号编码：仅 A-Z a-z 0-9 - _ . ~ 保留，其余逐字节转义（阿里云签名要求大写十六进制）。
fn percent_encode(text:&str)->String{
    let mut out=String::with_capacity(text.len());
    for byte in text.bytes(){
        match byte{
            b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'-'|b'_'|b'.'|b'~'=>out.push(byte as char),
            _=>{let _=write!(out,"%{byte:02X}");},
        }
    }
    out
}
use std::fmt::Write as _;

/// 阿里云 RPC V1 签名：按 key 排序拼 canonical query，
/// StringToSign = "POST&%2F&<encoded>"，HMAC-SHA1（密钥 = Secret + "&"）后 Base64。
fn signature(params:&[(String,String)],secret:&str)->String{
    let mut encoded:Vec<(String,String)>=params.iter().map(|(k,v)|(percent_encode(k),percent_encode(v))).collect();
    encoded.sort();
    let canonical=encoded.iter().map(|(k,v)|format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let to_sign=format!("POST&{}&{}",percent_encode("/"),percent_encode(&canonical));
    use base64::Engine;
    let key=ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,format!("{secret}&").as_bytes());
    base64::engine::general_purpose::STANDARD.encode(ring::hmac::sign(&key,to_sign.as_bytes()))
}

fn nonce()->String{
    use std::hash::{BuildHasher,Hasher};
    let nanos=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_nanos()).unwrap_or(0);
    let salt=std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("{nanos:x}-{salt:x}")
}

/// 单段正文翻译：构造签名请求并解析 Data.Translated。
fn request_chunk(agent:&ureq::Agent,ak_id:&str,secret:&str,text:&str)->Result<(String,String),String>{
    let timestamp=chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut params:Vec<(String,String)>=vec![
        ("AccessKeyId".into(),ak_id.into()),
        ("Action".into(),"TranslateGeneral".into()),
        ("FormatType".into(),"text".into()),
        ("Scene".into(),"general".into()),
        ("SignatureMethod".into(),"HMAC-SHA1".into()),
        ("SignatureNonce".into(),nonce()),
        ("SignatureVersion".into(),"1.0".into()),
        ("SourceLanguage".into(),"auto".into()),
        ("TargetLanguage".into(),"zh".into()),
        ("Timestamp".into(),timestamp),
        ("Version".into(),"2018-01-17".into()),
        ("SourceText".into(),text.into()),
    ];
    let sign=signature(&params,secret);
    params.push(("Signature".into(),sign));
    let body=params.iter().map(|(k,v)|format!("{}={}",percent_encode(k),percent_encode(v))).collect::<Vec<_>>().join("&");
    let response=agent.post(ENDPOINT)
        .set("Content-Type","application/x-www-form-urlencoded")
        .send_string(&body)
        .map_err(|e|match e{
            ureq::Error::Status(code,_)=>format!("翻译服务返回状态 {code}"),
            _=>"无法连接翻译服务，请检查网络".into(),
        })?;
    let mut raw=String::new();
    response.into_reader().take(2*1024*1024).read_to_string(&mut raw).map_err(|_|"读取翻译响应失败".to_string())?;
    let value:Value=serde_json::from_str(raw.trim()).map_err(|_|"翻译服务返回了无效数据".to_string())?;
    if let Some(code)=value["Code"].as_str().filter(|s|!s.is_empty()){
        let message=value["Message"].as_str().unwrap_or("未知错误");
        return Err(aliyun_error(code,message));
    }
    let translated=value["Data"]["Translated"].as_str().unwrap_or_default().trim().to_string();
    let detected=value["Data"]["DetectedLanguage"].as_str().unwrap_or_default().to_string();
    if translated.is_empty(){return Err("翻译服务没有返回译文".into());}
    Ok((translated,detected))
}

/// 常见错误码转成人话；鉴权类错误提示检查 AccessKey。
fn aliyun_error(code:&str,message:&str)->String{
    match code{
        c if c.starts_with("Throttling")=>"翻译请求过于频繁，请稍后再试".into(),
        "InvalidAccessKeyId.NotFound"|"SignatureDoesNotMatch"=>"AccessKey 无效，请检查 AccessKeyId 与 AccessKeySecret".into(),
        c if c.contains("LimitExceeded")||c.contains("Quota")=>format!("账号额度不足：{message}"),
        _=>format!("翻译失败（{code}）：{message}"),
    }
}

/// 按段落把正文切成不超过 CHUNK_BYTES 的块；单段超长时按字符边界硬切。
fn chunk_text(text:&str)->Vec<String>{
    let mut chunks=Vec::new();let mut current=String::new();
    for (i,part) in text.split('\n').enumerate(){
        // 按原始换行拼回：文本以 \n 结尾时不会多出尾巴。
        let line=if i==0{part.to_string()}else{format!("\n{part}")};
        if !current.is_empty() && current.len()+line.len()>CHUNK_BYTES{chunks.push(std::mem::take(&mut current));}
        if line.len()>CHUNK_BYTES{
            // 单段超长：按字符边界切片。
            let mut start=0;
            while start<line.len(){
                let mut end=(start+CHUNK_BYTES).min(line.len());
                while !line.is_char_boundary(end){end-=1;}
                chunks.push(line[start..end].to_string());start=end;
            }
        }else{current.push_str(&line);}
    }
    if !current.trim().is_empty(){chunks.push(current);}
    if chunks.is_empty(){chunks.push(text.to_string());}
    chunks
}

static ROTATE: AtomicUsize = AtomicUsize::new(0);

/// 整篇正文翻译：从轮询位置开始依次尝试启用的账号，失败自动换下一个。
pub fn translate_text(text:&str)->Result<String,String>{
    let text=text.trim();
    if text.is_empty(){return Err("没有需要翻译的内容".into());}
    let accounts:Vec<Account>=Config::load().accounts.into_iter().filter(|a|a.enabled).collect();
    if accounts.is_empty(){return Err("还没有配置翻译账号".into());}
    let start=ROTATE.fetch_add(1,Ordering::Relaxed)%accounts.len();
    let agent=ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20)).build();
    let chunks=chunk_text(text);
    let mut last_error=String::new();
    for offset in 0..accounts.len(){
        let account=&accounts[(start+offset)%accounts.len()];
        let secret=match secrets::load(&account.id){Ok(secret)=>secret,Err(e)=>{last_error=format!("{}：{e}",account.label);continue;}};
        let mut translated=Vec::with_capacity(chunks.len());
        let mut failure=None;
        for chunk in &chunks{
            match request_chunk(&agent,&account.ak_id,&secret,chunk){
                Ok((piece,detected))=>{
                    // 已经是中文就不送后续分块，直接视为无需翻译。
                    if detected.eq_ignore_ascii_case("zh"){translated=vec!["原文已是中文，无需翻译。".into()];break;}
                    translated.push(piece);
                }
                Err(e)=>{failure=Some(format!("{}：{e}",account.label));break;}
            }
        }
        if let Some(error)=failure{last_error=error;continue;}
        return Ok(translated.join("\n").trim().to_string());
    }
    Err(if last_error.is_empty(){"没有可用的翻译账号".into()}else{last_error})
}

// ---------------------------------------------------------------- App 界面状态

pub(crate) struct Done{pub account:String,pub uid:u32,pub result:Result<String,String>}

#[derive(Default)]
pub(crate) struct State {
    pub config:Config,
    pub form:Form,
    rx:Option<std::sync::mpsc::Receiver<Done>>,
    pub busy:HashSet<(String,u32)>,
    pub cache:HashMap<(String,u32),Result<String,String>>,
    /// 正文区当前显示译文还是原文；键与 cache 一致。
    pub show:HashMap<(String,u32),bool>,
}
#[derive(Default)]
pub(crate) struct Form {pub label:String, pub ak_id:String, pub ak_secret:String, pub show:bool}

impl State {
    pub fn load()->Self{Self{config:Config::load(),..Default::default()}}
}

pub(crate) enum View { NoAccounts, Hidden, Idle, Busy, Ready{shown:bool} }
impl Clone for View {fn clone(&self)->Self{*self}}
impl Copy for View {}

impl View {pub fn ready(&self)->bool{matches!(self,View::Ready{..})}}

impl crate::app::App {
    /// 邮件界面按钮该处于什么状态：没配账号时不显示入口，验证码遮蔽期禁止翻译。
    pub(crate) fn translate_view(&self,item:&crate::mail::Item)->View{
        if !self.translate.config.accounts.iter().any(|a|a.enabled){return View::NoAccounts;}
        let key=(item.account.clone(),item.uid);
        if self.mail_code_hidden(item){return View::Hidden;}
        if self.translate.busy.contains(&key){return View::Busy;}
        if self.translate.cache.contains_key(&key){return View::Ready{shown:*self.translate.show.get(&key).unwrap_or(&true)};}
        View::Idle
    }
    /// 发起后台翻译；同一封邮件同时只允许一个请求。
    pub(crate) fn translate_request(&mut self,item:&crate::mail::Item){
        let key=(item.account.clone(),item.uid);
        if self.translate.busy.contains(&key)||self.translate.cache.contains_key(&key){return;}
        if !self.translate.config.accounts.iter().any(|a|a.enabled){self.mail.note="请先在设置 → 邮箱里添加阿里云翻译账号".into();return;}
        let text=item.summary.trim().to_string();
        if text.is_empty(){self.translate.cache.insert(key.clone(),Err("这封邮件没有正文".into()));self.translate.show.insert(key,true);return;}
        let (tx,rx)=std::sync::mpsc::channel();
        self.translate.rx=Some(rx);
        self.translate.busy.insert(key.clone());
        let account=item.account.clone();let uid=item.uid;
        std::thread::spawn(move||{
            let result=translate_text(&text);
            let _=tx.send(Done{account,uid,result});
        });
    }
    /// 译文与原文切换。
    pub(crate) fn translate_toggle(&mut self,item:&crate::mail::Item){
        let key=(item.account.clone(),item.uid);
        if let Some(shown)=self.translate.show.get_mut(&key){*shown=!*shown;}
    }
    /// 丢弃旧译文并重新请求。
    pub(crate) fn translate_retry(&mut self,item:&crate::mail::Item){
        let key=(item.account.clone(),item.uid);
        self.translate.cache.remove(&key);self.translate.show.remove(&key);
        self.translate_request(item);
    }
    /// 每帧轮询后台结果（main.rs 调用）。
    pub fn poll_translate(&mut self){
        let Some(rx)=&self.translate.rx else{return;};
        while let Ok(done)=rx.try_recv(){
            let key=(done.account.clone(),done.uid);
            self.translate.busy.remove(&key);
            self.translate.cache.insert(key.clone(),done.result);
            self.translate.show.insert(key,true);
        }
        if self.translate.busy.is_empty(){self.translate.rx=None;}
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_follows_rfc3986(){
        assert_eq!(percent_encode("aZ09-_."), "aZ09-_.");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("中"), "%E4%B8%AD");
        assert_eq!(percent_encode("a+b/c"), "a%2Bb%2Fc");
    }

    #[test]
    fn chunk_respects_byte_limit_and_char_boundaries(){
        let short="第一段\n\n第二段\n".to_string();
        assert_eq!(chunk_text(&short),vec![short.clone()]);
        let long="长".repeat(CHUNK_BYTES+50);
        for chunk in chunk_text(&long){
            assert!(chunk.len()<=CHUNK_BYTES,"分块必须小于上限");
            assert!(chunk.chars().all(|c|c=='长'));
        }
        let total:usize=chunk_text(&long).iter().map(|c|c.chars().count()).sum();
        assert_eq!(total,long.chars().count(),"分块合并后不能丢字");
    }

    #[test]
    fn signature_is_deterministic_and_matches_reference_vector(){
        // 固定向量：参数顺序无关，相同输入必须得到相同签名。
        let params=vec![
            ("Timestamp".to_string(),"2026-11-07T02:00:00Z".to_string()),
            ("Action".to_string(),"TranslateGeneral".to_string()),
            ("AccessKeyId".to_string(),"testid".to_string()),
        ];
        let first=signature(&params,"testsecret");
        let mut shuffled=params.clone();shuffled.reverse();
        assert_eq!(signature(&shuffled,"testsecret"),first);
        // 外部参照：Python hashlib 对相同 StringToSign 的 HMAC-SHA1。
        // 阿里云规范对 canonical query 再做一次 RFC3986 编码（%3D→%253D 双重转义）。
        // StringToSign = POST&%2F&AccessKeyId%253Dtestid%2526Action%253DTranslateGeneral%2526Timestamp%253D2026-11-07T02%253A00%253A00Z
        assert_eq!(first,"qlzaq8t58thgbJh6Jp8sYBvdIg4=");
    }

    #[test]
    fn aliyun_error_maps_common_codes(){
        assert_eq!(aliyun_error("ThrottlingRequest","x"),"翻译请求过于频繁，请稍后再试");
        assert_eq!(aliyun_error("InvalidAccessKeyId.NotFound","x"),"AccessKey 无效，请检查 AccessKeyId 与 AccessKeySecret");
        assert!(aliyun_error("SomethingElse","详情").contains("SomethingElse"));
    }
}
