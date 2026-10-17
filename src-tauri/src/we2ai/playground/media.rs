//! AI 营销工坊 P2/P3：图片生成/编辑与视频生成（`docs/we2ai/AI营销工坊设计方案.md`
//! 第 6、7 节，功能 27 续）。
//!
//! 作为 `playground` 的子模块存在，直接复用其私有的任务注册表、身份监督
//! （[`supervised`]）、Key 解析与错误契约：
//! - 明文 Key 只在 Rust 侧取用，从不出 IPC；媒体文件由 Rust 落盘，前端只拿到
//!   路径与（图片的）data URL。
//! - 与文本对话共用 [`PlaygroundState`]（并发上限 1），登出 `cancel_all` 同样中断
//!   在途的图片/视频任务。
//! - 不读上游错误 body（可能回显 Key 明文），只按状态码映射稳定 code。
//!
//! 网关契约（来自 we2ai.com/docs，**未经实测**，联调前以真实响应为准）：
//! - 图片：`POST /v1/images/generations`（OpenAI/Grok，`data[].b64_json`）、
//!   `POST /v1/images/edits`（multipart）、`POST /v1beta/models/{m}:generateContent`
//!   （Gemini，`inlineData`）。
//! - 视频：`POST /v1/videos` → `GET /v1/videos/{id}` 轮询 → `GET /v1/videos/{id}/content`。
//!   Grok 与 Seedance 的请求体不同，按模型名分族（[`VideoFamily`]）。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine as _;
use serde_json::{json, Value};
use tauri::Emitter;

use super::*;

pub mod works;
pub(super) use works::WorkGuard;

/// 视频任务进度事件名（Rust → 前端）。守卫 4.19 校验两侧一致。
pub const PLAYGROUND_PROGRESS_EVENT: &str = "we2ai-playground-progress";

/// 提示词长度上限（字符）。
const MEDIA_MAX_PROMPT_CHARS: usize = 4000;
/// Grok 视频提示词字节上限：超过会被上游拒绝，建单前拒绝以免白等一轮。
const MEDIA_GROK_MAX_PROMPT_BYTES: usize = 4096;
/// 参考图解码后的字节上限。
const MEDIA_MAX_REFERENCE_BYTES: usize = 8 * 1024 * 1024;
const MEDIA_MAX_REFERENCE_TOTAL_BYTES: usize = 32 * 1024 * 1024;
/// 单张生成图字节上限。
const MEDIA_MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// 图片接口响应体上限（n=4 的 base64 合计）。
const MEDIA_MAX_IMAGE_RESPONSE_BYTES: usize = 96 * 1024 * 1024;
/// 视频文件字节上限。
const MEDIA_MAX_VIDEO_BYTES: usize = 200 * 1024 * 1024;
/// 图片请求总超时（含读 body）。
const MEDIA_IMAGE_TIMEOUT: Duration = Duration::from_secs(300);
/// 视频内容下载总超时。
const MEDIA_VIDEO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// 轮询连续网络失败容忍次数。
const MEDIA_POLL_MAX_NET_ERRORS: u32 = 3;

const CODE_REJECTED: &str = "PLAYGROUND_REJECTED";
const CODE_MEDIA_FAILED: &str = "PLAYGROUND_MEDIA_FAILED";
const CODE_MEDIA_TIMEOUT: &str = "PLAYGROUND_MEDIA_TIMEOUT";
const CODE_MEDIA_INVALID: &str = "PLAYGROUND_MEDIA_INVALID";
const CODE_PATH_INVALID: &str = "PLAYGROUND_PATH_INVALID";
const CODE_SAVE_FAILED: &str = "PLAYGROUND_SAVE_FAILED";

// ---------------------------------------------------------------------------
// 模型族与能力表（单一事实来源：前端只渲染这里下发的选项）
// ---------------------------------------------------------------------------

/// 一个模型族可用的参数选项。空切片表示该参数不适用（前端不渲染、后端拒绝下发）。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaCapabilities {
    pub sizes: &'static [&'static str],
    pub aspect_ratios: &'static [&'static str],
    pub image_sizes: &'static [&'static str],
    pub durations: &'static [u32],
    pub resolutions: &'static [&'static str],
    pub max_n: u32,
    pub supports_reference: bool,
    pub max_reference_images: usize,
    pub supports_audio: bool,
    /// 参考图宽高的下限，0 表示无要求。Seedance 实测：低于 300 像素会被
    /// 网关以 `InvalidParameter: image_url` 拒绝（2026-10-14）。
    pub min_reference_side: u32,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageFamily {
    Openai,
    Grok,
    Gemini,
}

impl ImageFamily {
    /// 按模型名分族：服务端 `kind` 只说明是图片，请求体形态只能靠名称区分。
    /// 未识别的图片模型按 OpenAI 兼容形态处理（网关的通用 `/v1/images/*`）。
    fn of(model: &str) -> Self {
        let lower = model.to_ascii_lowercase();
        if lower.contains("gemini") {
            Self::Gemini
        } else if lower.starts_with("grok") || lower.contains("grok-imagine") {
            Self::Grok
        } else {
            Self::Openai
        }
    }

    fn capabilities(self) -> MediaCapabilities {
        match self {
            // 尺寸表为 OpenAI 通用值，文档未列 gpt-image 的完整支持表【待联调】。
            Self::Openai => MediaCapabilities {
                sizes: &["1024x1024", "1536x1024", "1024x1536"],
                aspect_ratios: &[],
                image_sizes: &[],
                durations: &[],
                resolutions: &[],
                max_n: 4,
                supports_reference: true,
                max_reference_images: 1,
                supports_audio: false,
                min_reference_side: 0,
            },
            // 2026-10-14 实测：生成与编辑接口都按 `aspect_ratio` 出图（16:9 → 1280x720，
            // 1:1 → 1024x1024，4:3 → 1152x864，9:16 → 720x1280），`size` 被忽略；
            // 选中的 Grok 模型带参考图时直接走 `/v1/images/edits`，不需要切到
            // `grok-imagine-edit`。`n=2` 返回 2 张；4 张未实测。
            Self::Grok => MediaCapabilities {
                sizes: &[],
                aspect_ratios: &["1:1", "16:9", "9:16", "4:3", "3:4"],
                image_sizes: &[],
                durations: &[],
                resolutions: &[],
                max_n: 4,
                supports_reference: true,
                max_reference_images: 1,
                supports_audio: false,
                min_reference_side: 0,
            },
            Self::Gemini => MediaCapabilities {
                sizes: &[],
                aspect_ratios: &["1:1", "3:4", "4:3", "9:16", "16:9"],
                image_sizes: &["1K", "2K", "4K"],
                durations: &[],
                resolutions: &[],
                max_n: 1,
                supports_reference: true,
                max_reference_images: 1,
                supports_audio: false,
                min_reference_side: 0,
            },
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Grok => "grok",
            Self::Gemini => "gemini",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VideoFamily {
    Grok,
    Seedance,
}

impl VideoFamily {
    /// 视频模型没有通用请求体，名称不属于已知两族的一律不展示（猜错会白白计费）。
    fn of(model: &str) -> Option<Self> {
        let lower = model.to_ascii_lowercase();
        if lower.contains("seedance") {
            Some(Self::Seedance)
        } else if lower.starts_with("grok") || lower.contains("grok-imagine") {
            Some(Self::Grok)
        } else {
            None
        }
    }

    /// 2026-10-14 逐项核对（参数非法会在创建时被 400/422 拒绝，合法则通过）：
    /// - Grok：时长 5·10 秒、画幅 16:9·9:16·1:1、分辨率 480p·720p 均合法；
    /// - Seedance mini：时长 5·10·15 秒、五种画幅、480p·720p 合法，**1080p 被拒**
    ///   （其他 Seedance 型号未测，保守不开放 1080p）。
    fn capabilities(self) -> MediaCapabilities {
        match self {
            Self::Grok => MediaCapabilities {
                sizes: &[],
                aspect_ratios: &["16:9", "9:16", "1:1"],
                image_sizes: &[],
                durations: &[5, 10],
                resolutions: &["480p", "720p"],
                max_n: 1,
                supports_reference: true,
                max_reference_images: 1,
                supports_audio: false,
                min_reference_side: 0,
            },
            Self::Seedance => MediaCapabilities {
                sizes: &[],
                aspect_ratios: &["16:9", "9:16", "1:1", "4:3", "3:4"],
                image_sizes: &[],
                durations: &[5, 10, 15],
                resolutions: &["480p", "720p"],
                max_n: 1,
                supports_reference: true,
                max_reference_images: 1,
                supports_audio: true,
                min_reference_side: 300,
            },
        }
    }

    fn capabilities_for_model(self, model: &str) -> MediaCapabilities {
        let mut caps = self.capabilities();
        let lower = model.to_ascii_lowercase();
        caps.max_reference_images = match self {
            Self::Grok if lower.contains("video-1.5") => 14,
            Self::Grok => 7,
            Self::Seedance if lower.contains("seedance-2.5") => 30,
            Self::Seedance if lower.contains("seedance-2.0") => 9,
            // Older / unversioned aliases retain their known first-frame contract.
            Self::Seedance => 1,
        };
        caps
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::Seedance => "seedance",
        }
    }
}

// ---------------------------------------------------------------------------
// 请求 / 视图结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundReferenceImage {
    pub mime: String,
    /// 纯 base64（允许带 `data:...;base64,` 前缀，解码前会剥掉）。
    pub data_base64: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundImageRequest {
    pub request_id: String,
    pub key_id: i64,
    pub model: String,
    pub prompt: String,
    pub n: Option<u32>,
    pub size: Option<String>,
    pub aspect_ratio: Option<String>,
    pub image_size: Option<String>,
    pub reference: Option<PlaygroundReferenceImage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundVideoRequest {
    pub request_id: String,
    pub key_id: i64,
    pub model: String,
    pub prompt: String,
    pub duration: Option<u32>,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    pub generate_audio: Option<bool>,
    pub reference: Option<PlaygroundReferenceImage>,
    /// New reference-to-video input; legacy `reference` retains first-frame semantics.
    #[serde(default)]
    pub references: Vec<PlaygroundReferenceImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundMediaFile {
    pub path: String,
    pub mime: String,
    pub bytes: u64,
    /// 仅图片带（前端 CSP 允许 `data:` 图片）；视频体积大，只给路径。
    pub data_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundImageOutcome {
    pub images: Vec<PlaygroundMediaFile>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundVideoOutcome {
    pub video: PlaygroundMediaFile,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundMediaModelView {
    pub id: String,
    pub provider: Option<String>,
    pub family: &'static str,
    pub capabilities: MediaCapabilities,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundMediaModelsView {
    pub models: Vec<PlaygroundMediaModelView>,
    pub blocked_reason: Option<String>,
}

/// 默认优先 Grok：它在 we2ai 上图片/视频都最便宜。前端默认选中列表第一项，所以
/// 这里把 Grok 族稳定地排到最前，其余保持服务端原有顺序。
fn prefer_cheapest_family(models: &mut [PlaygroundMediaModelView]) {
    models.sort_by_key(|m| if m.family == "grok" { 0 } else { 1 });
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaygroundProgressPayload {
    request_id: String,
    stage: &'static str,
    elapsed_secs: u64,
    progress: Option<u32>,
}

// ---------------------------------------------------------------------------
// 校验与请求体构造（纯函数）
// ---------------------------------------------------------------------------

fn bad_request(message: impl Into<String>) -> We2aiApiError {
    playground_error(CODE_BAD_REQUEST, message)
}

fn check_request_id(request_id: &str) -> Result<(), We2aiApiError> {
    if request_id.trim().is_empty() || request_id.len() > PLAYGROUND_MAX_REQUEST_ID_LEN {
        return Err(bad_request("请求标识无效"));
    }
    Ok(())
}

fn check_prompt(prompt: &str) -> Result<String, We2aiApiError> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err(bad_request("提示词不能为空"));
    }
    if trimmed.chars().count() > MEDIA_MAX_PROMPT_CHARS {
        return Err(playground_error(
            CODE_PAYLOAD_TOO_LARGE,
            format!("提示词过长（上限 {MEDIA_MAX_PROMPT_CHARS} 字），请精简后重试"),
        ));
    }
    Ok(trimmed.to_string())
}

/// 取值必须在能力表里（空表 = 该参数不适用，传了就拒绝）。
fn check_choice<'a>(
    name: &str,
    value: Option<&'a str>,
    allowed: &[&str],
) -> Result<Option<&'a str>, We2aiApiError> {
    match value {
        None => Ok(None),
        Some(v) if allowed.contains(&v) => Ok(Some(v)),
        Some(_) => Err(bad_request(format!("{name} 取值不受该模型支持"))),
    }
}

/// 进入 URL 路径的片段（模型名、上游任务 id）只允许安全字符，防路径注入。
fn is_path_safe(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 128
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn sniff_video_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        Some("video/mp4")
    } else if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Some("video/webm")
    } else {
        None
    }
}

fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        _ => "bin",
    }
}

/// 读取 PNG / JPEG / WebP 的像素宽高（只解析头部，不解码图像）。
fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let le16 = |b: &[u8]| u32::from(u16::from_le_bytes([b[0], b[1]]));
    let le24 = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], 0]);
    match sniff_image_mime(bytes)? {
        "image/png" => (bytes.len() >= 24).then(|| (be32(&bytes[16..20]), be32(&bytes[20..24]))),
        "image/jpeg" => {
            let mut i = 2;
            while i + 9 < bytes.len() {
                if bytes[i] != 0xFF {
                    i += 1;
                    continue;
                }
                let marker = bytes[i + 1];
                // SOF0..SOF15（排除 DHT=C4、JPG=C8、DAC=CC）携带尺寸。
                if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
                    let height = u32::from(u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]));
                    let width = u32::from(u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]));
                    return Some((width, height));
                }
                let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
                i += 2 + len;
            }
            None
        }
        "image/webp" => {
            if bytes.len() < 30 {
                return None;
            }
            match &bytes[12..16] {
                b"VP8X" => Some((le24(&bytes[24..27]) + 1, le24(&bytes[27..30]) + 1)),
                b"VP8L" if bytes[20] == 0x2F => {
                    let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
                    Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
                }
                b"VP8 " => Some((le16(&bytes[26..28]) & 0x3FFF, le16(&bytes[28..30]) & 0x3FFF)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn decode_base64_lenient(raw: &str) -> Option<Vec<u8>> {
    let stripped = match raw.strip_prefix("data:") {
        Some(rest) => rest.split_once(',').map(|(_, b)| b).unwrap_or(rest),
        None => raw,
    };
    base64::engine::general_purpose::STANDARD
        .decode(stripped.trim())
        .ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferenceImage {
    mime: &'static str,
    bytes: Vec<u8>,
}

impl ReferenceImage {
    fn data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.mime,
            base64::engine::general_purpose::STANDARD.encode(&self.bytes)
        )
    }
}

/// 参考图校验：声明的 mime 与真实字节头都必须是 png/jpeg/webp，且不超限。
fn decode_reference(
    reference: &PlaygroundReferenceImage,
    min_side: u32,
) -> Result<ReferenceImage, We2aiApiError> {
    let bytes = decode_base64_lenient(&reference.data_base64)
        .ok_or_else(|| bad_request("参考图数据无效"))?;
    if bytes.is_empty() || bytes.len() > MEDIA_MAX_REFERENCE_BYTES {
        return Err(playground_error(
            CODE_PAYLOAD_TOO_LARGE,
            format!(
                "参考图过大（上限 {} MB），请压缩后重试",
                MEDIA_MAX_REFERENCE_BYTES / 1024 / 1024
            ),
        ));
    }
    let sniffed = sniff_image_mime(&bytes)
        .ok_or_else(|| bad_request("参考图格式不支持（仅支持 PNG、JPEG、WebP）"))?;
    if reference.mime != sniffed {
        return Err(bad_request("参考图格式与文件内容不一致"));
    }
    if min_side > 0 {
        match image_dimensions(&bytes) {
            Some((w, h)) if w >= min_side && h >= min_side => {}
            Some((w, h)) => {
                return Err(bad_request(format!(
                    "该模型要求参考图宽高都不小于 {min_side} 像素（当前 {w}×{h}），请换一张更大的图"
                )));
            }
            None => return Err(bad_request("无法读取参考图尺寸，请换一张图")),
        }
    }
    Ok(ReferenceImage {
        mime: sniffed,
        bytes,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImagePlan {
    family: ImageFamily,
    model: String,
    prompt: String,
    n: u32,
    size: Option<String>,
    aspect_ratio: Option<String>,
    image_size: Option<String>,
    reference: Option<ReferenceImage>,
}

fn validate_image_request(request: &PlaygroundImageRequest) -> Result<ImagePlan, We2aiApiError> {
    check_request_id(&request.request_id)?;
    let model = request.model.trim();
    if model.is_empty() {
        return Err(bad_request("未选择模型"));
    }
    let family = ImageFamily::of(model);
    // Gemini 的模型名进 URL 路径，必须是安全片段。
    if family == ImageFamily::Gemini && !is_path_safe(model) {
        return Err(bad_request("模型名包含不支持的字符"));
    }
    let caps = family.capabilities();
    let prompt = check_prompt(&request.prompt)?;
    let n = request.n.unwrap_or(1);
    if n == 0 || n > caps.max_n {
        return Err(bad_request(format!("该模型一次最多生成 {} 张", caps.max_n)));
    }
    let size = check_choice("size", request.size.as_deref(), caps.sizes)?.map(str::to_string);
    let aspect_ratio = check_choice(
        "aspectRatio",
        request.aspect_ratio.as_deref(),
        caps.aspect_ratios,
    )?
    .map(str::to_string);
    let image_size = check_choice("imageSize", request.image_size.as_deref(), caps.image_sizes)?
        .map(str::to_string);
    let reference = match &request.reference {
        None => None,
        Some(_) if !caps.supports_reference => {
            return Err(bad_request("该模型不支持参考图编辑"));
        }
        Some(reference) => Some(decode_reference(reference, caps.min_reference_side)?),
    };
    Ok(ImagePlan {
        family,
        model: model.to_string(),
        prompt,
        n,
        size,
        aspect_ratio,
        image_size,
        reference,
    })
}

/// 图片请求的 HTTP 形态。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ImageCall {
    Json {
        path: String,
        body: Value,
    },
    Multipart {
        path: String,
        boundary: String,
        body: Vec<u8>,
    },
}

fn build_multipart(
    boundary: &str,
    fields: &[(&str, String)],
    file_field: &str,
    filename: &str,
    mime: &str,
    data: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 1024);
    for (name, value) in fields {
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    out.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(data);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

fn build_image_call(plan: &ImagePlan, boundary: &str) -> ImageCall {
    match plan.family {
        ImageFamily::Openai => {
            if let Some(reference) = &plan.reference {
                let mut fields = vec![
                    ("model", plan.model.clone()),
                    ("prompt", plan.prompt.clone()),
                    ("n", plan.n.to_string()),
                ];
                if let Some(size) = &plan.size {
                    fields.push(("size", size.clone()));
                }
                ImageCall::Multipart {
                    path: "/v1/images/edits".to_string(),
                    boundary: boundary.to_string(),
                    body: build_multipart(
                        boundary,
                        &fields,
                        "image",
                        &format!("reference.{}", extension_for(reference.mime)),
                        reference.mime,
                        &reference.bytes,
                    ),
                }
            } else {
                let mut body = json!({
                    "model": plan.model,
                    "prompt": plan.prompt,
                    "n": plan.n,
                });
                if let Some(size) = &plan.size {
                    body["size"] = json!(size);
                }
                ImageCall::Json {
                    path: "/v1/images/generations".to_string(),
                    body,
                }
            }
        }
        ImageFamily::Grok => {
            if let Some(reference) = &plan.reference {
                let mut fields = vec![
                    ("model", plan.model.clone()),
                    ("prompt", plan.prompt.clone()),
                    ("n", plan.n.to_string()),
                    ("response_format", "b64_json".to_string()),
                ];
                if let Some(ratio) = &plan.aspect_ratio {
                    fields.push(("aspect_ratio", ratio.clone()));
                }
                ImageCall::Multipart {
                    path: "/v1/images/edits".to_string(),
                    boundary: boundary.to_string(),
                    body: build_multipart(
                        boundary,
                        &fields,
                        "image",
                        &format!("reference.{}", extension_for(reference.mime)),
                        reference.mime,
                        &reference.bytes,
                    ),
                }
            } else {
                let mut body = json!({
                    "model": plan.model,
                    "prompt": plan.prompt,
                    "n": plan.n,
                    "response_format": "b64_json",
                });
                if let Some(ratio) = &plan.aspect_ratio {
                    body["aspect_ratio"] = json!(ratio);
                }
                ImageCall::Json {
                    path: "/v1/images/generations".to_string(),
                    body,
                }
            }
        }
        ImageFamily::Gemini => {
            let mut parts = vec![json!({ "text": plan.prompt })];
            if let Some(reference) = &plan.reference {
                parts.push(json!({
                    "inlineData": {
                        "mimeType": reference.mime,
                        "data": base64::engine::general_purpose::STANDARD.encode(&reference.bytes),
                    }
                }));
            }
            let mut image_config = serde_json::Map::new();
            if let Some(ratio) = &plan.aspect_ratio {
                image_config.insert("aspectRatio".to_string(), json!(ratio));
            }
            if let Some(size) = &plan.image_size {
                image_config.insert("imageSize".to_string(), json!(size));
            }
            let mut generation_config = json!({ "responseModalities": ["TEXT", "IMAGE"] });
            if !image_config.is_empty() {
                generation_config["imageConfig"] = Value::Object(image_config);
            }
            ImageCall::Json {
                path: format!("/v1beta/models/{}:generateContent", plan.model),
                body: json!({
                    "contents": [{ "parts": parts }],
                    "generationConfig": generation_config,
                }),
            }
        }
    }
}

/// 响应里的一张图：内联字节，或需另行下载的临时 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ImageItem {
    Bytes(Vec<u8>),
    Url(String),
}

fn parse_images(family: ImageFamily, value: &Value) -> Vec<ImageItem> {
    let mut items = Vec::new();
    match family {
        ImageFamily::Openai | ImageFamily::Grok => {
            for entry in value["data"].as_array().into_iter().flatten() {
                if let Some(b64) = entry["b64_json"].as_str() {
                    if let Some(bytes) = decode_base64_lenient(b64) {
                        items.push(ImageItem::Bytes(bytes));
                    }
                } else if let Some(url) = entry["url"].as_str() {
                    items.push(ImageItem::Url(url.to_string()));
                }
            }
        }
        ImageFamily::Gemini => {
            for candidate in value["candidates"].as_array().into_iter().flatten() {
                for part in candidate["content"]["parts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let inline = if part["inlineData"].is_object() {
                        &part["inlineData"]
                    } else {
                        &part["inline_data"]
                    };
                    if let Some(data) = inline["data"].as_str() {
                        if let Some(bytes) = decode_base64_lenient(data) {
                            items.push(ImageItem::Bytes(bytes));
                        }
                    }
                }
            }
        }
    }
    items
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VideoPlan {
    family: VideoFamily,
    model: String,
    prompt: String,
    duration: Option<u32>,
    aspect_ratio: Option<String>,
    resolution: Option<String>,
    generate_audio: bool,
    reference: Option<ReferenceImage>,
    references: Vec<ReferenceImage>,
}

fn validate_video_request(request: &PlaygroundVideoRequest) -> Result<VideoPlan, We2aiApiError> {
    check_request_id(&request.request_id)?;
    let model = request.model.trim();
    if model.is_empty() {
        return Err(bad_request("未选择模型"));
    }
    let family =
        VideoFamily::of(model).ok_or_else(|| bad_request("暂不支持该视频模型的请求格式"))?;
    let caps = family.capabilities_for_model(model);
    let prompt = check_prompt(&request.prompt)?;
    if family == VideoFamily::Grok && prompt.len() > MEDIA_GROK_MAX_PROMPT_BYTES {
        return Err(playground_error(
            CODE_PAYLOAD_TOO_LARGE,
            format!("该模型提示词不能超过 {MEDIA_GROK_MAX_PROMPT_BYTES} 字节，请精简后重试"),
        ));
    }
    let duration = match request.duration {
        None => None,
        Some(d) if caps.durations.contains(&d) => Some(d),
        Some(_) => return Err(bad_request("duration 取值不受该模型支持")),
    };
    let aspect_ratio = check_choice(
        "aspectRatio",
        request.aspect_ratio.as_deref(),
        caps.aspect_ratios,
    )?
    .map(str::to_string);
    let resolution = check_choice(
        "resolution",
        request.resolution.as_deref(),
        caps.resolutions,
    )?
    .map(str::to_string);
    let generate_audio = request.generate_audio.unwrap_or(false);
    if generate_audio && !caps.supports_audio {
        return Err(bad_request("该模型不支持生成音频"));
    }
    if request.reference.is_some() && !request.references.is_empty() {
        return Err(bad_request("首帧图与多图参考不能同时提交"));
    }
    if request.references.len() > caps.max_reference_images {
        return Err(bad_request(format!(
            "该模型最多支持 {} 张参考图",
            caps.max_reference_images
        )));
    }
    // Bound the encoded payload before decoding; 32 MB raw stays below a 64 MB JSON body.
    let encoded_bytes: usize = request.references.iter().map(|r| r.data_base64.len()).sum();
    if encoded_bytes
        > MEDIA_MAX_REFERENCE_TOTAL_BYTES.div_ceil(3) * 4 + request.references.len() * 128
    {
        return Err(bad_request("参考图总大小不能超过 32 MB"));
    }
    let mut references = Vec::with_capacity(request.references.len());
    let mut total_bytes = 0;
    for raw in &request.references {
        let image = decode_reference(raw, caps.min_reference_side)?;
        total_bytes += image.bytes.len();
        if total_bytes > MEDIA_MAX_REFERENCE_TOTAL_BYTES {
            return Err(bad_request("参考图总大小不能超过 32 MB"));
        }
        references.push(image);
    }
    let reference = match &request.reference {
        None => None,
        Some(reference) => Some(decode_reference(reference, caps.min_reference_side)?),
    };
    Ok(VideoPlan {
        family,
        model: model.to_string(),
        prompt,
        duration,
        aspect_ratio,
        resolution,
        generate_audio,
        reference,
        references,
    })
}

fn build_video_body(plan: &VideoPlan) -> Value {
    match plan.family {
        VideoFamily::Grok => {
            let mut body = json!({ "model": plan.model, "prompt": plan.prompt });
            if let Some(duration) = plan.duration {
                body["duration"] = json!(duration);
            }
            if let Some(ratio) = &plan.aspect_ratio {
                body["aspect_ratio"] = json!(ratio);
            }
            if let Some(resolution) = &plan.resolution {
                body["resolution"] = json!(resolution);
            }
            if let Some(reference) = &plan.reference {
                body["image"] = json!({ "url": reference.data_url(), "type": "image_url" });
            }
            if !plan.references.is_empty() {
                body["reference_images"] = json!(plan
                    .references
                    .iter()
                    .map(|image| json!({ "url": image.data_url() }))
                    .collect::<Vec<_>>());
            }
            body
        }
        VideoFamily::Seedance => {
            let mut content = vec![json!({ "type": "text", "text": plan.prompt })];
            if let Some(reference) = &plan.reference {
                content.push(json!({
                    "type": "image_url",
                    "image_url": { "url": reference.data_url() },
                    "role": "first_frame",
                }));
            }
            let role = if plan
                .family
                .capabilities_for_model(&plan.model)
                .max_reference_images
                > 1
            {
                "reference_image"
            } else {
                "first_frame"
            };
            for image in &plan.references {
                content.push(json!({
                    "type": "image_url",
                    "image_url": { "url": image.data_url() },
                    "role": role,
                }));
            }
            let mut body = json!({
                "model": plan.model,
                "content": content,
                "watermark": false,
                "generate_audio": plan.generate_audio,
            });
            if let Some(duration) = plan.duration {
                body["duration"] = json!(duration);
            }
            if let Some(ratio) = &plan.aspect_ratio {
                body["ratio"] = json!(ratio);
            }
            if let Some(resolution) = &plan.resolution {
                body["resolution"] = json!(resolution);
            }
            body
        }
    }
}

/// 创建响应里的任务 id：Grok 文档为 `request_id`，Seedance 为 `id`；兼容 `task_id`
/// 与 `data.*` 嵌套。id 要拼进轮询路径，必须通过安全片段校验。
fn parse_video_task_id(value: &Value) -> Option<String> {
    const KEYS: [&str; 3] = ["request_id", "id", "task_id"];
    let pick = |v: &Value| KEYS.iter().find_map(|k| v[*k].as_str().map(str::to_string));
    pick(value)
        .or_else(|| pick(&value["data"]))
        .filter(|id| is_path_safe(id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoState {
    Running(Option<u32>),
    Succeeded,
    Failed,
}

/// 轮询状态映射：文档只给了 `pending → … → succeeded`（Grok）与
/// `queued → running → succeeded/failed`（Seedance），中间值未穷举，所以未知值一律
/// 视为进行中，靠总等待上限兜底。
fn parse_video_state(value: &Value) -> VideoState {
    let status = ["status", "state"]
        .iter()
        .find_map(|k| value[*k].as_str().or_else(|| value["data"][*k].as_str()))
        .unwrap_or("")
        .to_ascii_lowercase();
    match status.as_str() {
        "succeeded" | "success" | "completed" | "complete" | "done" => VideoState::Succeeded,
        "failed" | "failure" | "error" | "cancelled" | "canceled" | "expired" | "rejected" => {
            VideoState::Failed
        }
        _ => {
            let progress = value["progress"]
                .as_u64()
                .or_else(|| value["data"]["progress"].as_u64())
                .map(|p| p.min(100) as u32);
            VideoState::Running(progress)
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP 执行（不依赖 Tauri / SessionManager，wiremock 端到端测试）
// ---------------------------------------------------------------------------

fn media_status_code(status: u16) -> &'static str {
    match status {
        400 | 422 => CODE_REJECTED,
        413 => CODE_PAYLOAD_TOO_LARGE,
        other => upstream_status_code(other),
    }
}

fn media_message(code: &str) -> &'static str {
    match code {
        CODE_REJECTED => "请求被网关拒绝（参数不受支持或内容未通过审核），请调整提示词或参数后重试",
        CODE_PAYLOAD_TOO_LARGE => "提交内容过大，请精简提示词或压缩参考图后重试",
        CODE_MEDIA_FAILED => "生成失败（可能是提示词或参考图未通过审核），请调整后重试",
        CODE_MEDIA_TIMEOUT => "生成超时，请稍后重试；若已扣费请联系客服",
        CODE_MEDIA_INVALID => "网关返回的内容不是有效的图片或视频，请稍后重试",
        other => upstream_message(other),
    }
}

fn media_error(code: &str) -> We2aiApiError {
    playground_error(code, media_message(code))
}

struct MediaEnv<'a> {
    http: reqwest::Client,
    base_url: String,
    api_key: &'a str,
    cancel: Arc<Notify>,
    identity_ok: &'a (dyn Fn() -> bool + Send + Sync),
    /// 仅测试放开：生产只允许从 https 临时地址取图。
    allow_http_urls: bool,
}

impl MediaEnv<'_> {
    /// 发请求并映射：监督（取消/身份）→ 网络错误 → 非 2xx 状态码。不读错误 body。
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, We2aiApiError> {
        let response = match supervised(&self.cancel, self.identity_ok, request.send()).await {
            Err(err) => return Err(err),
            Ok(Ok(response)) => response,
            Ok(Err(e)) => {
                log::warn!("playground-media: 请求发送失败: {e}");
                return Err(playground_error(CODE_NETWORK, "网络连接失败，请稍后重试"));
            }
        };
        let status = response.status();
        if !status.is_success() {
            log::warn!("playground-media: 上游返回 {}", status.as_u16());
            return Err(media_error(media_status_code(status.as_u16())));
        }
        Ok(response)
    }

    /// 带上限地读完 body；整个读取过程受取消/身份监督。
    async fn read_capped(
        &self,
        response: reqwest::Response,
        cap: usize,
    ) -> Result<Vec<u8>, We2aiApiError> {
        self.read_capped_with_progress(response, cap, &mut |_| {})
            .await
    }

    /// 复用字节上限与取消/身份监督，仅在收到真实数据后回报下载进度。
    async fn read_capped_with_progress(
        &self,
        response: reqwest::Response,
        cap: usize,
        on_bytes: &mut (dyn FnMut(usize) + Send),
    ) -> Result<Vec<u8>, We2aiApiError> {
        let read = async move {
            let mut stream = response.bytes_stream();
            let mut out: Vec<u8> = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| CODE_NETWORK)?;
                if out.len() + chunk.len() > cap {
                    return Err(CODE_PAYLOAD_TOO_LARGE);
                }
                out.extend_from_slice(&chunk);
                on_bytes(out.len());
            }
            Ok(out)
        };
        match supervised(&self.cancel, self.identity_ok, read).await? {
            Ok(bytes) => Ok(bytes),
            Err(CODE_PAYLOAD_TOO_LARGE) => Err(media_error(CODE_MEDIA_INVALID)),
            Err(code) => Err(playground_error(code, "网络连接中断，请稍后重试")),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodedMedia {
    bytes: Vec<u8>,
    mime: &'static str,
}

fn into_image(bytes: Vec<u8>) -> Result<DecodedMedia, We2aiApiError> {
    if bytes.len() > MEDIA_MAX_IMAGE_BYTES {
        return Err(media_error(CODE_MEDIA_INVALID));
    }
    let mime = sniff_image_mime(&bytes).ok_or_else(|| media_error(CODE_MEDIA_INVALID))?;
    Ok(DecodedMedia { bytes, mime })
}

async fn run_image(
    env: &MediaEnv<'_>,
    plan: &ImagePlan,
    boundary: &str,
) -> Result<Vec<DecodedMedia>, We2aiApiError> {
    let builder = match build_image_call(plan, boundary) {
        ImageCall::Json { path, body } => env
            .http
            .post(env.url(&path))
            .bearer_auth(env.api_key)
            .timeout(MEDIA_IMAGE_TIMEOUT)
            .json(&body),
        ImageCall::Multipart {
            path,
            boundary,
            body,
        } => env
            .http
            .post(env.url(&path))
            .bearer_auth(env.api_key)
            .timeout(MEDIA_IMAGE_TIMEOUT)
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body),
    };
    // 文档（we2ai.com/docs）的 Gemini 原生接口用 `x-goog-api-key` 鉴权，未声明接受
    // Bearer；两个头都带，发往的是同一个网关主机。
    let builder = if plan.family == ImageFamily::Gemini {
        builder.header("x-goog-api-key", env.api_key)
    } else {
        builder
    };
    let response = env.send(builder).await?;
    let raw = env
        .read_capped(response, MEDIA_MAX_IMAGE_RESPONSE_BYTES)
        .await?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| media_error(CODE_MEDIA_INVALID))?;
    let items = parse_images(plan.family, &value);
    if items.is_empty() {
        return Err(media_error(CODE_MEDIA_INVALID));
    }
    let mut images = Vec::with_capacity(items.len());
    for item in items {
        match item {
            ImageItem::Bytes(bytes) => images.push(into_image(bytes)?),
            ImageItem::Url(url) => images.push(download_image_url(env, &url).await?),
        }
    }
    Ok(images)
}

/// 取临时 URL 的图片：不带 Authorization（URL 可能指向网关以外的主机，不能把 Key
/// 发过去），仅 https。
async fn download_image_url(env: &MediaEnv<'_>, url: &str) -> Result<DecodedMedia, We2aiApiError> {
    let parsed = url::Url::parse(url).map_err(|_| media_error(CODE_MEDIA_INVALID))?;
    let scheme_ok =
        parsed.scheme() == "https" || (env.allow_http_urls && parsed.scheme() == "http");
    if !scheme_ok || parsed.host_str().is_none() {
        return Err(media_error(CODE_MEDIA_INVALID));
    }
    let response = env
        .send(env.http.get(parsed).timeout(MEDIA_IMAGE_TIMEOUT))
        .await?;
    let bytes = env.read_capped(response, MEDIA_MAX_IMAGE_BYTES).await?;
    into_image(bytes)
}

#[derive(Debug, Clone, Copy)]
struct VideoTiming {
    poll_interval: Duration,
    max_wait: Duration,
    download_timeout: Duration,
}

impl Default for VideoTiming {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            max_wait: Duration::from_secs(15 * 60),
            download_timeout: MEDIA_VIDEO_DOWNLOAD_TIMEOUT,
        }
    }
}

struct VideoProgress {
    stage: &'static str,
    elapsed_secs: u64,
    progress: Option<u32>,
}

async fn run_video(
    env: &MediaEnv<'_>,
    plan: &VideoPlan,
    timing: VideoTiming,
    on_progress: &mut (dyn FnMut(VideoProgress) + Send),
) -> Result<DecodedMedia, We2aiApiError> {
    let started = tokio::time::Instant::now();
    let elapsed = || started.elapsed().as_secs();

    // 提交：非幂等且计费，不自动重放。
    let submit = env
        .send(
            env.http
                .post(env.url("/v1/videos"))
                .bearer_auth(env.api_key)
                .timeout(Duration::from_secs(120))
                .json(&build_video_body(plan)),
        )
        .await?;
    let raw = env.read_capped(submit, 1024 * 1024).await?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| media_error(CODE_MEDIA_INVALID))?;
    let task_id = parse_video_task_id(&value).ok_or_else(|| media_error(CODE_MEDIA_INVALID))?;
    on_progress(VideoProgress {
        stage: "submitted",
        elapsed_secs: elapsed(),
        progress: None,
    });

    let mut net_errors = 0u32;
    loop {
        if started.elapsed() > timing.max_wait {
            return Err(media_error(CODE_MEDIA_TIMEOUT));
        }
        supervised(
            &env.cancel,
            env.identity_ok,
            tokio::time::sleep(timing.poll_interval),
        )
        .await?;

        let poll = env
            .send(
                env.http
                    .get(env.url(&format!("/v1/videos/{task_id}")))
                    .bearer_auth(env.api_key)
                    .timeout(Duration::from_secs(60)),
            )
            .await;
        let response = match poll {
            Ok(response) => response,
            // 轮询期间偶发的网络错误不应放弃一个已计费的任务；鉴权/额度类状态码立即终止。
            Err(err) if err.code == CODE_NETWORK && net_errors < MEDIA_POLL_MAX_NET_ERRORS => {
                net_errors += 1;
                continue;
            }
            Err(err) => return Err(err),
        };
        let body = match env.read_capped(response, 1024 * 1024).await {
            Ok(body) => body,
            Err(err) if err.code == CODE_NETWORK && net_errors < MEDIA_POLL_MAX_NET_ERRORS => {
                net_errors += 1;
                continue;
            }
            Err(err) => return Err(err),
        };
        net_errors = 0;
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match parse_video_state(&value) {
            VideoState::Running(progress) => on_progress(VideoProgress {
                stage: "running",
                elapsed_secs: elapsed(),
                progress,
            }),
            VideoState::Failed => return Err(media_error(CODE_MEDIA_FAILED)),
            VideoState::Succeeded => break,
        }
    }

    on_progress(VideoProgress {
        stage: "downloading",
        elapsed_secs: elapsed(),
        progress: None,
    });
    let download = async {
        let response = env
            .send(
                env.http
                    .get(env.url(&format!("/v1/videos/{task_id}/content")))
                    .bearer_auth(env.api_key),
            )
            .await?;
        let total_bytes = response.content_length().filter(|n| *n > 0);
        let mut last_progress = None;
        let mut on_bytes = |received: usize| {
            // 未读到 EOF 前最多显示 99%，没有 Content-Length 时不编造百分比。
            let progress = total_bytes
                .map(|total| ((received as u64).saturating_mul(100) / total).min(99) as u32);
            if progress != last_progress {
                last_progress = progress;
                on_progress(VideoProgress {
                    stage: "downloading",
                    elapsed_secs: elapsed(),
                    progress,
                });
            }
        };
        let bytes = env
            .read_capped_with_progress(response, MEDIA_MAX_VIDEO_BYTES, &mut on_bytes)
            .await?;
        let mime = sniff_video_mime(&bytes).ok_or_else(|| media_error(CODE_MEDIA_INVALID))?;
        on_progress(VideoProgress {
            stage: "saving",
            elapsed_secs: elapsed(),
            progress: None,
        });
        Ok(DecodedMedia { bytes, mime })
    };
    tokio::time::timeout(timing.download_timeout, download)
        .await
        .map_err(|_| media_error(CODE_MEDIA_TIMEOUT))?
}

// ---------------------------------------------------------------------------
// 落盘与路径校验
// ---------------------------------------------------------------------------

fn studio_dir(data_root: &Path) -> PathBuf {
    data_root.join("studio")
}

fn sanitize_id(request_id: &str) -> String {
    request_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(40)
        .collect()
}

fn save_media(
    dir: &Path,
    request_id: &str,
    index: usize,
    media: &DecodedMedia,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!(
        "{epoch}-{}-{}.{}",
        sanitize_id(request_id),
        index + 1,
        extension_for(media.mime)
    );
    let path = dir.join(name);
    // 只把完整文件发布为作品；下载/写盘中的 .part 不会出现在列表里。
    let partial = path.with_extension(format!("{}.part", extension_for(media.mime)));
    if let Err(error) =
        std::fs::write(&partial, &media.bytes).and_then(|_| std::fs::rename(&partial, &path))
    {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }
    Ok(path)
}

/// 未建索引的历史媒体文件；列表来自持久化文件，不依赖前端会话。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundWork {
    pub name: String,
    pub saved_at_ms: u64,
    pub file: PlaygroundMediaFile,
}

fn list_studio_works(data_root: &Path) -> std::io::Result<Vec<PlaygroundWork>> {
    use std::io::Read;
    let entries = match std::fs::read_dir(studio_dir(data_root)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut works = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_file())
            || !matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("mp4" | "webm" | "png" | "jpg" | "jpeg" | "webp")
            )
        {
            continue;
        }
        let Ok(path) = resolve_studio_path(data_root, &path.to_string_lossy()) else {
            continue;
        };
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        let mut header = [0u8; 64];
        let Ok(read) = file.read(&mut header) else {
            continue;
        };
        let Some(mime) =
            sniff_video_mime(&header[..read]).or_else(|| sniff_image_mime(&header[..read]))
        else {
            continue;
        };
        works.push(PlaygroundWork {
            name: entry.file_name().to_string_lossy().into_owned(),
            saved_at_ms: metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            file: PlaygroundMediaFile {
                path: path.to_string_lossy().into_owned(),
                mime: mime.to_string(),
                bytes: metadata.len(),
                data_url: None,
            },
        });
    }
    works.sort_by(|a, b| {
        b.saved_at_ms
            .cmp(&a.saved_at_ms)
            .then_with(|| b.name.cmp(&a.name))
    });
    Ok(works)
}

/// 只接受工坊目录内、真实存在的文件；符号链接与 `..` 经 canonicalize 后再比较前缀。
fn resolve_studio_path(data_root: &Path, raw: &str) -> Result<PathBuf, We2aiApiError> {
    let invalid = || playground_error(CODE_PATH_INVALID, "文件不存在或不在工坊目录内");
    let dir = studio_dir(data_root)
        .canonicalize()
        .map_err(|_| invalid())?;
    let path = Path::new(raw).canonicalize().map_err(|_| invalid())?;
    if !path.starts_with(&dir) || !path.is_file() {
        return Err(invalid());
    }
    Ok(path)
}

fn media_file_view(path: &Path, media: &DecodedMedia, with_data_url: bool) -> PlaygroundMediaFile {
    PlaygroundMediaFile {
        path: path.to_string_lossy().into_owned(),
        mime: media.mime.to_string(),
        bytes: media.bytes.len() as u64,
        data_url: with_data_url.then(|| {
            format!(
                "data:{};base64,{}",
                media.mime,
                base64::engine::general_purpose::STANDARD.encode(&media.bytes)
            )
        }),
    }
}

// ---------------------------------------------------------------------------
// Tauri 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn we2ai_playground_media_models(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    key_id: i64,
    kind: String,
) -> Result<PlaygroundMediaModelsView, We2aiApiError> {
    if kind != "image" && kind != "video" {
        return Err(bad_request("kind 只能是 image 或 video"));
    }
    let manager = session.0.clone();
    let remote = super::super::keys::key_models(&manager, keys.inner(), key_id).await?;
    let mut models: Vec<PlaygroundMediaModelView> = remote
        .models
        .into_iter()
        .filter(|m| m.kind.as_deref() == Some(kind.as_str()))
        .filter_map(|m| {
            let (family, capabilities) = if kind == "image" {
                let family = ImageFamily::of(&m.id);
                (family.as_str(), family.capabilities())
            } else {
                // 视频只展示能识别请求体的模型族。
                let family = VideoFamily::of(&m.id)?;
                (family.as_str(), family.capabilities_for_model(&m.id))
            };
            Some(PlaygroundMediaModelView {
                id: m.id,
                provider: m.provider,
                family,
                capabilities,
            })
        })
        .collect();
    prefer_cheapest_family(&mut models);
    Ok(PlaygroundMediaModelsView {
        models,
        blocked_reason: remote.blocked_reason,
    })
}

/// 登记任务 + 取 Key + 构造监督闭包的公共前置。返回 (identity, secret, cancel, token)。
fn begin_media_task(
    manager: &SessionManager,
    keys: &We2aiKeyState,
    playground: &PlaygroundState,
    request_id: &str,
    key_id: i64,
) -> Result<(SessionIdentity, String, Arc<Notify>, u64), We2aiApiError> {
    let (identity, secret) = resolve_active_key(manager, keys, key_id)?;
    match playground.register(request_id, identity) {
        RegisterOutcome::Registered { cancel, token } => Ok((identity, secret, cancel, token)),
        RegisterOutcome::RequestIdInUse => Err(playground_error(
            CODE_REQUEST_ID_IN_USE,
            "请求标识重复，请重新发送",
        )),
        RegisterOutcome::Busy => Err(playground_error(
            CODE_BUSY,
            "已有生成任务进行中，请等待完成或先停止",
        )),
    }
}

#[tauri::command]
pub async fn we2ai_playground_image(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    playground: State<'_, PlaygroundState>,
    request: PlaygroundImageRequest,
) -> Result<PlaygroundImageOutcome, We2aiApiError> {
    let manager = session.0.clone();
    // 校验失败直接返回：不登记任务、不触碰 Key 缓存。
    let plan = validate_image_request(&request)?;
    let (identity, secret, cancel, token) = begin_media_task(
        &manager,
        keys.inner(),
        playground.inner(),
        &request.request_id,
        request.key_id,
    )?;
    let _guard = TaskGuard {
        state: playground.inner(),
        request_id: request.request_id.clone(),
        token,
    };
    let mut work = WorkGuard::start(
        manager.data_root(),
        &request.request_id,
        "image",
        &request.model,
        &request.prompt,
    )?;
    let outcome = async {
        let identity_manager = manager.clone();
        let identity_ok = move || identity_manager.current_identity() == Some(identity);
        let env = MediaEnv {
            http: PLAYGROUND_HTTP_CLIENT.clone(),
            base_url: identity.region.base_url().to_string(),
            api_key: &secret,
            cancel,
            identity_ok: &identity_ok,
            allow_http_urls: false,
        };
        let boundary = format!("----we2ai{}", uuid::Uuid::new_v4().simple());
        let images = run_image(&env, &plan, &boundary).await?;

        let dir = studio_dir(manager.data_root());
        let request_id = request.request_id.clone();
        let saved = tauri::async_runtime::spawn_blocking(move || {
            images
                .iter()
                .enumerate()
                .map(|(i, media)| {
                    save_media(&dir, &request_id, i, media)
                        .map(|path| media_file_view(&path, media, true))
                })
                .collect::<std::io::Result<Vec<_>>>()
        })
        .await
        .map_err(|e| playground_error(CODE_SAVE_FAILED, format!("保存图片失败: {e}")))?
        .map_err(|e| playground_error(CODE_SAVE_FAILED, format!("保存图片失败: {e}")))?;
        Ok(PlaygroundImageOutcome { images: saved })
    }
    .await;
    match &outcome {
        Ok(o) => work.finish(Ok(o.images.clone()), None)?,
        Err(e) => work.finish(Err(e), None)?,
    }
    outcome
}

#[tauri::command]
pub async fn we2ai_playground_video(
    app: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    playground: State<'_, PlaygroundState>,
    request: PlaygroundVideoRequest,
) -> Result<PlaygroundVideoOutcome, We2aiApiError> {
    let manager = session.0.clone();
    let plan = validate_video_request(&request)?;
    let (identity, secret, cancel, token) = begin_media_task(
        &manager,
        keys.inner(),
        playground.inner(),
        &request.request_id,
        request.key_id,
    )?;
    let _guard = TaskGuard {
        state: playground.inner(),
        request_id: request.request_id.clone(),
        token,
    };
    let mut work = WorkGuard::start(
        manager.data_root(),
        &request.request_id,
        "video",
        &request.model,
        &request.prompt,
    )?;
    let outcome = async {
        let identity_manager = manager.clone();
        let identity_ok = move || identity_manager.current_identity() == Some(identity);
        let env = MediaEnv {
            http: PLAYGROUND_HTTP_CLIENT.clone(),
            base_url: identity.region.base_url().to_string(),
            api_key: &secret,
            cancel,
            identity_ok: &identity_ok,
            allow_http_urls: false,
        };
        let emit_request_id = request.request_id.clone();
        let mut on_progress = |p: VideoProgress| {
            work.progress(p.progress);
            let _ = app.emit(
                PLAYGROUND_PROGRESS_EVENT,
                PlaygroundProgressPayload {
                    request_id: emit_request_id.clone(),
                    stage: p.stage,
                    elapsed_secs: p.elapsed_secs,
                    progress: p.progress,
                },
            );
        };
        let video = run_video(&env, &plan, VideoTiming::default(), &mut on_progress).await?;

        let dir = studio_dir(manager.data_root());
        let request_id = request.request_id.clone();
        let view = tauri::async_runtime::spawn_blocking(move || {
            save_media(&dir, &request_id, 0, &video)
                .map(|path| media_file_view(&path, &video, false))
        })
        .await
        .map_err(|e| playground_error(CODE_SAVE_FAILED, format!("保存视频失败: {e}")))?
        .map_err(|e| playground_error(CODE_SAVE_FAILED, format!("保存视频失败: {e}")))?;
        Ok(PlaygroundVideoOutcome { video: view })
    }
    .await;
    match &outcome {
        Ok(o) => work.finish(Ok(vec![o.video.clone()]), None)?,
        Err(e) => work.finish(Err(e), None)?,
    }
    outcome
}

/// 另存为：弹出系统保存对话框，把工坊目录内的文件复制到用户选择的位置。
/// 返回目标路径；用户取消返回 `None`。
#[tauri::command]
pub async fn we2ai_playground_media_save_as(
    app: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    path: String,
) -> Result<Option<String>, We2aiApiError> {
    use tauri_plugin_dialog::DialogExt;

    let source = resolve_studio_path(session.0.data_root(), &path)?;
    let default_name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "we2ai-media".to_string());
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_file_name(&default_name)
            .blocking_save_file()
    })
    .await
    .map_err(|e| playground_error(CODE_SAVE_FAILED, format!("打开保存对话框失败: {e}")))?;
    let Some(target) = picked else {
        return Ok(None);
    };
    let target = target
        .into_path()
        .map_err(|e| playground_error(CODE_SAVE_FAILED, format!("保存位置无效: {e}")))?;
    std::fs::copy(&source, &target)
        .map_err(|e| playground_error(CODE_SAVE_FAILED, format!("保存失败: {e}")))?;
    Ok(Some(target.to_string_lossy().into_owned()))
}

/// 用系统默认程序打开，或在文件夹中显示。
#[tauri::command]
pub async fn we2ai_playground_media_open(
    app: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    path: String,
    reveal: bool,
) -> Result<(), We2aiApiError> {
    use tauri_plugin_opener::OpenerExt;

    let target = resolve_studio_path(session.0.data_root(), &path)?;
    let result = if reveal {
        app.opener().reveal_item_in_dir(&target)
    } else {
        app.opener()
            .open_path(target.to_string_lossy().into_owned(), None::<&str>)
    };
    result.map_err(|e| {
        log::warn!("playground-media: 打开文件失败: {e}");
        playground_error(CODE_SAVE_FAILED, "无法打开该文件")
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
    const MP4: &[u8] = &[
        0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'm', b'p', b'4', b'2', 0, 0,
    ];

    /// 只含签名与 IHDR 的 PNG 头（`image_dimensions` 只读头部）。
    fn png_of(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]);
        v
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn reference(bytes: &[u8], mime: &str) -> PlaygroundReferenceImage {
        PlaygroundReferenceImage {
            mime: mime.to_string(),
            data_base64: b64(bytes),
        }
    }

    fn image_request(model: &str) -> PlaygroundImageRequest {
        PlaygroundImageRequest {
            request_id: "req-img".to_string(),
            key_id: 7,
            model: model.to_string(),
            prompt: "CNC machine, studio shot".to_string(),
            n: None,
            size: None,
            aspect_ratio: None,
            image_size: None,
            reference: None,
        }
    }

    fn video_request(model: &str) -> PlaygroundVideoRequest {
        PlaygroundVideoRequest {
            request_id: "req-vid".to_string(),
            key_id: 7,
            model: model.to_string(),
            prompt: "factory walkthrough".to_string(),
            duration: None,
            aspect_ratio: None,
            resolution: None,
            generate_audio: None,
            reference: None,
            references: vec![],
        }
    }

    fn always_ok() -> impl Fn() -> bool + Send + Sync {
        || true
    }

    // ---- 分族与能力 ----

    #[test]
    fn image_family_detected_by_model_name() {
        assert_eq!(
            ImageFamily::of("gpt-image-2.5-sunburst"),
            ImageFamily::Openai
        );
        assert_eq!(ImageFamily::of("grok-imagine-image-2.0"), ImageFamily::Grok);
        assert_eq!(
            ImageFamily::of("gemini-3.1-flash-image"),
            ImageFamily::Gemini
        );
        assert_eq!(ImageFamily::of("some-unknown-image"), ImageFamily::Openai);
    }

    #[test]
    fn grok_models_sort_first_and_others_keep_their_order() {
        let view = |id: &str| {
            let family = ImageFamily::of(id);
            PlaygroundMediaModelView {
                id: id.to_string(),
                provider: None,
                family: family.as_str(),
                capabilities: family.capabilities(),
            }
        };
        let mut models = vec![
            view("gpt-image-2"),
            view("gemini-3.1-flash-image"),
            view("grok-imagine-image-2.0"),
            view("dall-e-3"),
        ];
        prefer_cheapest_family(&mut models);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "grok-imagine-image-2.0",
                "gpt-image-2",
                "gemini-3.1-flash-image",
                "dall-e-3"
            ]
        );
    }

    #[test]
    fn video_family_unknown_models_are_not_offered() {
        assert_eq!(
            VideoFamily::of("grok-imagine-video-1.5"),
            Some(VideoFamily::Grok)
        );
        assert_eq!(
            VideoFamily::of("doubao-seedance-2.0-fast"),
            Some(VideoFamily::Seedance)
        );
        assert_eq!(VideoFamily::of("sora-2"), None);
    }

    // ---- 图片校验与请求体 ----

    #[test]
    fn image_validation_rejects_unsupported_params() {
        let mut r = image_request("grok-imagine-image-2.0");
        r.size = Some("1024x1024".to_string());
        assert!(validate_image_request(&r).is_err(), "grok 不支持 size");

        let mut r = image_request("gemini-3.1-flash-image");
        r.n = Some(2);
        assert!(validate_image_request(&r).is_err(), "gemini 只能 1 张");

        let mut r = image_request("grok-imagine-image-2.0");
        r.aspect_ratio = Some("5:4".to_string());
        assert!(
            validate_image_request(&r).is_err(),
            "画幅不在 grok 能力表内"
        );

        let mut r = image_request("gpt-image-2");
        r.prompt = "   ".to_string();
        assert!(validate_image_request(&r).is_err());

        let mut r = image_request("gpt-image-2");
        r.request_id = String::new();
        assert!(validate_image_request(&r).is_err());
    }

    #[test]
    fn reference_must_match_declared_mime_and_limits() {
        let mut r = image_request("gpt-image-2");
        r.reference = Some(reference(PNG, "image/jpeg"));
        assert!(validate_image_request(&r).is_err(), "声明与内容不一致");

        r.reference = Some(reference(b"not an image at all", "image/png"));
        assert!(validate_image_request(&r).is_err(), "非图片字节");

        let big = vec![0u8; MEDIA_MAX_REFERENCE_BYTES + 1];
        let mut padded = PNG.to_vec();
        padded.extend_from_slice(&big);
        r.reference = Some(reference(&padded, "image/png"));
        let err = validate_image_request(&r).unwrap_err();
        assert_eq!(err.code, CODE_PAYLOAD_TOO_LARGE);

        r.reference = Some(PlaygroundReferenceImage {
            mime: "image/png".to_string(),
            data_base64: format!("data:image/png;base64,{}", b64(PNG)),
        });
        assert!(validate_image_request(&r).is_ok(), "允许 data URL 前缀");
    }

    #[test]
    fn gemini_model_with_path_chars_is_rejected() {
        let r = image_request("gemini/../../admin");
        assert!(validate_image_request(&r).is_err());
    }

    #[test]
    fn image_call_shapes_per_family() {
        let plan = validate_image_request(&{
            let mut r = image_request("gpt-image-2");
            r.size = Some("1024x1024".to_string());
            r.n = Some(2);
            r
        })
        .unwrap();
        match build_image_call(&plan, "B") {
            ImageCall::Json { path, body } => {
                assert_eq!(path, "/v1/images/generations");
                assert_eq!(body["n"], 2);
                assert_eq!(body["size"], "1024x1024");
            }
            other => panic!("unexpected {other:?}"),
        }

        let plan = validate_image_request(&{
            let mut r = image_request("grok-imagine-image-2.0");
            r.aspect_ratio = Some("16:9".to_string());
            r
        })
        .unwrap();
        match build_image_call(&plan, "B") {
            ImageCall::Json { path, body } => {
                assert_eq!(path, "/v1/images/generations");
                assert_eq!(body["response_format"], "b64_json");
                assert_eq!(body["aspect_ratio"], "16:9");
                assert!(body.get("size").is_none());
            }
            other => panic!("unexpected {other:?}"),
        }

        // 带参考图：同一个 Grok 模型改走 multipart 编辑接口，画幅随表单下发。
        let plan = validate_image_request(&{
            let mut r = image_request("grok-imagine-image-2.0");
            r.aspect_ratio = Some("4:3".to_string());
            r.reference = Some(reference(PNG, "image/png"));
            r
        })
        .unwrap();
        match build_image_call(&plan, "BOUND") {
            ImageCall::Multipart { path, body, .. } => {
                assert_eq!(path, "/v1/images/edits");
                let text = String::from_utf8_lossy(&body);
                assert!(text.contains("name=\"model\"\r\n\r\ngrok-imagine-image-2.0"));
                assert!(text.contains("name=\"response_format\"\r\n\r\nb64_json"));
                assert!(text.contains("name=\"aspect_ratio\"\r\n\r\n4:3"));
            }
            other => panic!("unexpected {other:?}"),
        }

        let plan = validate_image_request(&{
            let mut r = image_request("gemini-3.1-flash-image");
            r.aspect_ratio = Some("16:9".to_string());
            r.image_size = Some("2K".to_string());
            r.reference = Some(reference(PNG, "image/png"));
            r
        })
        .unwrap();
        match build_image_call(&plan, "B") {
            ImageCall::Json { path, body } => {
                assert_eq!(
                    path,
                    "/v1beta/models/gemini-3.1-flash-image:generateContent"
                );
                assert_eq!(
                    body["generationConfig"]["imageConfig"]["aspectRatio"],
                    "16:9"
                );
                assert_eq!(body["generationConfig"]["imageConfig"]["imageSize"], "2K");
                assert_eq!(body["contents"][0]["parts"].as_array().unwrap().len(), 2);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn openai_reference_becomes_multipart_edit() {
        let plan = validate_image_request(&{
            let mut r = image_request("gpt-image-2");
            r.reference = Some(reference(PNG, "image/png"));
            r
        })
        .unwrap();
        match build_image_call(&plan, "BOUND") {
            ImageCall::Multipart {
                path,
                boundary,
                body,
            } => {
                assert_eq!(path, "/v1/images/edits");
                assert_eq!(boundary, "BOUND");
                let text = String::from_utf8_lossy(&body);
                assert!(text.contains("name=\"prompt\""));
                assert!(text.contains("name=\"image\"; filename=\"reference.png\""));
                assert!(text.contains("Content-Type: image/png"));
                assert!(text.ends_with("--BOUND--\r\n"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parse_images_openai_and_gemini() {
        let v = json!({"data":[{"b64_json": b64(PNG)},{"url":"https://x/y.png"}]});
        let items = parse_images(ImageFamily::Openai, &v);
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], ImageItem::Bytes(b) if b == PNG));
        assert!(matches!(&items[1], ImageItem::Url(u) if u == "https://x/y.png"));

        let v = json!({"candidates":[{"content":{"parts":[
            {"text":"ok"},
            {"inlineData":{"mimeType":"image/png","data": b64(PNG)}}
        ]}}]});
        assert_eq!(parse_images(ImageFamily::Gemini, &v).len(), 1);
        let v = json!({"candidates":[{"content":{"parts":[
            {"inline_data":{"mime_type":"image/png","data": b64(PNG)}}
        ]}}]});
        assert_eq!(parse_images(ImageFamily::Gemini, &v).len(), 1);
    }

    #[test]
    fn image_dimensions_for_png_jpeg_and_webp_variants() {
        assert_eq!(image_dimensions(&png_of(640, 480)), Some((640, 480)));

        // JPEG：SOI + APP0(16 字节段) + SOF0(高 480, 宽 640)。
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpeg.extend_from_slice(&[0u8; 14]);
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 8]);
        jpeg.extend_from_slice(&480u16.to_be_bytes());
        jpeg.extend_from_slice(&640u16.to_be_bytes());
        jpeg.extend_from_slice(&[0u8; 12]);
        assert_eq!(image_dimensions(&jpeg), Some((640, 480)));

        // WebP VP8X：宽高各减 1，24 位小端。
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        vp8x.extend_from_slice(&[10, 0, 0, 0, 0, 0, 0, 0]);
        vp8x.extend_from_slice(&(799u32).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(599u32).to_le_bytes()[..3]);
        assert_eq!(image_dimensions(&vp8x), Some((800, 600)));

        // WebP VP8L：14 位宽、14 位高（均减 1）。
        let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L".to_vec();
        vp8l.extend_from_slice(&[5, 0, 0, 0, 0x2F]);
        vp8l.extend_from_slice(&((399u32) | (299u32 << 14)).to_le_bytes());
        vp8l.extend_from_slice(&[0u8; 6]);
        assert_eq!(image_dimensions(&vp8l), Some((400, 300)));

        // WebP VP8（有损）：帧头后是 14 位宽高。
        let mut vp8 = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        vp8.extend_from_slice(&[10, 0, 0, 0, 0, 0, 0, 0x9D, 0x01, 0x2A]);
        vp8.extend_from_slice(&512u16.to_le_bytes());
        vp8.extend_from_slice(&384u16.to_le_bytes());
        assert_eq!(image_dimensions(&vp8), Some((512, 384)));

        assert_eq!(image_dimensions(b"not an image"), None);
        assert_eq!(
            image_dimensions(&[0xFF, 0xD8, 0xFF]),
            None,
            "截断的 JPEG 不得越界"
        );
    }

    /// 实测（2026-10-14）：Seedance 首帧图宽或高低于 300 像素会被网关拒绝。
    #[test]
    fn seedance_rejects_small_reference_but_grok_does_not() {
        let mut r = video_request("doubao-seedance-2.0-mini");
        r.reference = Some(reference(&png_of(256, 256), "image/png"));
        let err = validate_video_request(&r).unwrap_err();
        assert_eq!(err.code, CODE_BAD_REQUEST);
        assert!(
            err.message.contains("300") && err.message.contains("256×256"),
            "{}",
            err.message
        );

        r.reference = Some(reference(&png_of(300, 299), "image/png"));
        assert!(validate_video_request(&r).is_err(), "任一边不足都拒绝");
        r.reference = Some(reference(&png_of(300, 300), "image/png"));
        assert!(validate_video_request(&r).is_ok(), "恰好 300 通过");

        let mut g = video_request("grok-imagine-video-1.5");
        g.reference = Some(reference(&png_of(64, 64), "image/png"));
        assert!(validate_video_request(&g).is_ok(), "Grok 没有最小边长要求");
        assert_eq!(VideoFamily::Seedance.capabilities().min_reference_side, 300);
        assert_eq!(VideoFamily::Grok.capabilities().min_reference_side, 0);
    }

    #[test]
    fn sniffing_identifies_formats() {
        assert_eq!(sniff_image_mime(PNG), Some("image/png"));
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_image_mime(b"RIFFxxxxWEBPyyyy"), Some("image/webp"));
        assert_eq!(sniff_image_mime(b"<html>"), None);
        assert_eq!(sniff_video_mime(MP4), Some("video/mp4"));
        assert_eq!(sniff_video_mime(b"hello"), None);
    }

    // ---- 视频校验、请求体、状态 ----

    #[test]
    fn video_validation_and_bodies() {
        assert!(validate_video_request(&video_request("sora-2")).is_err());

        let mut r = video_request("grok-imagine-video-1.5");
        r.duration = Some(7);
        assert!(validate_video_request(&r).is_err(), "时长不在能力表");
        r.duration = Some(5);
        r.generate_audio = Some(true);
        assert!(validate_video_request(&r).is_err(), "grok 不支持音频");

        let mut r = video_request("grok-imagine-video-1.5");
        r.duration = Some(5);
        r.aspect_ratio = Some("16:9".to_string());
        r.resolution = Some("480p".to_string());
        r.reference = Some(reference(PNG, "image/png"));
        let body = build_video_body(&validate_video_request(&r).unwrap());
        assert_eq!(body["duration"], 5);
        assert_eq!(body["aspect_ratio"], "16:9");
        assert_eq!(body["resolution"], "480p");
        assert_eq!(body["image"]["type"], "image_url");
        assert!(body["image"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));

        let mut r = video_request("doubao-seedance-2.0");
        r.duration = Some(15);
        r.aspect_ratio = Some("9:16".to_string());
        r.resolution = Some("720p".to_string());
        r.generate_audio = Some(true);
        r.reference = Some(reference(&png_of(640, 480), "image/png"));
        let body = build_video_body(&validate_video_request(&r).unwrap());
        assert_eq!(body["content"][0]["type"], "text");
        assert_eq!(body["content"][1]["role"], "first_frame");
        assert_eq!(body["ratio"], "9:16");
        assert_eq!(body["generate_audio"], true);
        assert_eq!(body["watermark"], false);
    }

    #[test]
    fn video_reference_arrays_preserve_order_and_native_roles() {
        let first = png_of(640, 480);
        let second = png_of(800, 600);
        for model in [
            "grok-imagine-video-1.5",
            "doubao-seedance-2.0",
            "doubao-seedance-2.5",
        ] {
            let mut request = video_request(model);
            request.references = vec![
                reference(&first, "image/png"),
                reference(&second, "image/png"),
            ];
            let body = build_video_body(&validate_video_request(&request).unwrap());
            if model.starts_with("grok") {
                let images = body["reference_images"].as_array().unwrap();
                assert_eq!(images.len(), 2);
                assert_eq!(
                    images[0]["url"],
                    format!("data:image/png;base64,{}", b64(&first))
                );
                assert_eq!(
                    images[1]["url"],
                    format!("data:image/png;base64,{}", b64(&second))
                );
                assert!(body.get("image").is_none());
            } else {
                let content = body["content"].as_array().unwrap();
                assert_eq!(content.len(), 3);
                for (part, bytes) in content[1..].iter().zip([&first, &second]) {
                    assert_eq!(part["role"], "reference_image");
                    assert_eq!(
                        part["image_url"]["url"],
                        format!("data:image/png;base64,{}", b64(bytes))
                    );
                }
            }
        }
        let mut legacy = video_request("seedance-1.0-pro");
        legacy.references = vec![reference(&first, "image/png")];
        assert_eq!(
            build_video_body(&validate_video_request(&legacy).unwrap())["content"][1]["role"],
            "first_frame"
        );
    }

    #[test]
    fn video_reference_limits_and_every_image_are_validated_before_submit() {
        let image = reference(&png_of(300, 300), "image/png");
        for (model, max) in [
            ("grok-imagine-video", 7),
            ("grok-imagine-video-1.5", 14),
            ("doubao-seedance-2.0-mini", 9),
            ("doubao-seedance-2.5", 30),
            ("seedance-1.0-pro", 1),
        ] {
            let mut request = video_request(model);
            request.references = vec![image.clone(); max];
            assert!(
                validate_video_request(&request).is_ok(),
                "{model}: limit boundary"
            );
            request.references.push(image.clone());
            assert!(validate_video_request(&request)
                .unwrap_err()
                .message
                .contains("最多支持"));
        }
        let mut request = video_request("doubao-seedance-2.0");
        request.references = vec![image.clone(), reference(&png_of(299, 300), "image/png")];
        assert!(validate_video_request(&request)
            .unwrap_err()
            .message
            .contains("299×300"));
        request.references[1] = reference(PNG, "image/jpeg");
        assert!(
            validate_video_request(&request).is_err(),
            "MIME mismatch on second image"
        );
        request.references[1] = PlaygroundReferenceImage {
            mime: "image/png".into(),
            data_base64: "invalid".into(),
        };
        assert!(
            validate_video_request(&request).is_err(),
            "invalid second image"
        );
        request.reference = Some(image);
        assert!(validate_video_request(&request)
            .unwrap_err()
            .message
            .contains("不能同时提交"));
        let mut request = video_request("grok-imagine-video");
        let mut bytes = PNG.to_vec();
        bytes.resize(MEDIA_MAX_REFERENCE_BYTES, 0);
        request.references = vec![reference(&bytes, "image/png"); 4];
        assert!(validate_video_request(&request).is_ok(), "exactly 32 MB");
        request.references.push(reference(PNG, "image/png"));
        assert!(validate_video_request(&request)
            .unwrap_err()
            .message
            .contains("32 MB"));
        request.references.push(reference(&bytes, "image/png"));
        assert!(
            validate_video_request(&request)
                .unwrap_err()
                .message
                .contains("32 MB"),
            "encoded preflight"
        );
    }

    #[test]
    fn grok_prompt_over_4096_bytes_is_rejected_before_submit() {
        let mut r = video_request("grok-imagine-video-1.5");
        r.prompt = "机".repeat(1400); // 4200 字节，字符数未超 4000
        let err = validate_video_request(&r).unwrap_err();
        assert_eq!(err.code, CODE_PAYLOAD_TOO_LARGE);
        let mut r = video_request("doubao-seedance-2.0");
        r.prompt = "机".repeat(1400);
        assert!(
            validate_video_request(&r).is_ok(),
            "seedance 不受 4096 字节限制"
        );
    }

    #[test]
    fn video_task_id_and_state_parsing() {
        assert_eq!(
            parse_video_task_id(&json!({"request_id":"abc-1"})).as_deref(),
            Some("abc-1")
        );
        assert_eq!(
            parse_video_task_id(&json!({"id":"cgt-2024"})).as_deref(),
            Some("cgt-2024")
        );
        assert_eq!(
            parse_video_task_id(&json!({"data":{"id":"x1"}})).as_deref(),
            Some("x1")
        );
        assert_eq!(parse_video_task_id(&json!({"id":"../etc/passwd"})), None);
        assert_eq!(parse_video_task_id(&json!({})), None);

        assert_eq!(
            parse_video_state(&json!({"status":"succeeded"})),
            VideoState::Succeeded
        );
        assert_eq!(
            parse_video_state(&json!({"status":"FAILED"})),
            VideoState::Failed
        );
        assert_eq!(
            parse_video_state(&json!({"status":"queued"})),
            VideoState::Running(None)
        );
        assert_eq!(
            parse_video_state(&json!({"status":"running","progress":42})),
            VideoState::Running(Some(42))
        );
        assert_eq!(
            parse_video_state(&json!({"data":{"status":"completed"}})),
            VideoState::Succeeded
        );
        assert_eq!(parse_video_state(&Value::Null), VideoState::Running(None));
    }

    // ---- 落盘与路径 ----

    #[test]
    fn save_media_writes_and_resolves_only_inside_studio_dir() {
        let root = tempfile::tempdir().unwrap();
        let dir = studio_dir(root.path());
        let media = DecodedMedia {
            bytes: PNG.to_vec(),
            mime: "image/png",
        };
        let path = save_media(&dir, "req/../x y", 0, &media).unwrap();
        assert!(path.starts_with(&dir));
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-reqxy-1.png"));

        let ok = resolve_studio_path(root.path(), path.to_str().unwrap()).unwrap();
        assert_eq!(ok, path.canonicalize().unwrap());

        let outside = root.path().join("secret.txt");
        std::fs::write(&outside, "x").unwrap();
        assert_eq!(
            resolve_studio_path(root.path(), outside.to_str().unwrap())
                .unwrap_err()
                .code,
            CODE_PATH_INVALID
        );
        let traversal = format!("{}/../secret.txt", dir.display());
        assert!(resolve_studio_path(root.path(), &traversal).is_err());
        assert!(
            resolve_studio_path(root.path(), dir.to_str().unwrap()).is_err(),
            "目录本身不行"
        );
    }

    // ---- 端到端（wiremock） ----

    fn env<'a>(
        server: &wiremock::MockServer,
        identity_ok: &'a (dyn Fn() -> bool + Send + Sync),
        cancel: Arc<Notify>,
    ) -> MediaEnv<'a> {
        MediaEnv {
            http: reqwest::Client::new(),
            base_url: server.uri(),
            api_key: "sk-test-secret",
            cancel,
            identity_ok,
            allow_http_urls: true,
        }
    }

    fn fast() -> VideoTiming {
        VideoTiming {
            poll_interval: Duration::from_millis(20),
            max_wait: Duration::from_secs(5),
            download_timeout: MEDIA_VIDEO_DOWNLOAD_TIMEOUT,
        }
    }

    #[tokio::test]
    async fn image_generation_returns_decoded_images() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .and(header("authorization", "Bearer sk-test-secret"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data":[{"b64_json": b64(PNG)},{"b64_json": b64(PNG)}]})),
            )
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_image_request(&{
            let mut r = image_request("gpt-image-2");
            r.n = Some(2);
            r
        })
        .unwrap();
        let images = run_image(&env(&server, &ok, Arc::new(Notify::new())), &plan, "B")
            .await
            .unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].mime, "image/png");
    }

    #[tokio::test]
    async fn gemini_image_sends_google_api_key_header_and_parses_inline_data() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/v1beta/models/gemini-3.1-flash-image:generateContent",
            ))
            .and(header("x-goog-api-key", "sk-test-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [
                    {"text": "here you go"},
                    {"inlineData": {"mimeType": "image/png", "data": b64(PNG)}}
                ]}}]
            })))
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_image_request(&image_request("gemini-3.1-flash-image")).unwrap();
        let images = run_image(&env(&server, &ok, Arc::new(Notify::new())), &plan, "B")
            .await
            .unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime, "image/png");
    }

    #[tokio::test]
    async fn non_gemini_image_does_not_send_google_header() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":[{"b64_json": b64(PNG)}]})),
            )
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_image_request(&image_request("gpt-image-2")).unwrap();
        run_image(&env(&server, &ok, Arc::new(Notify::new())), &plan, "B")
            .await
            .unwrap();
        let received = server.received_requests().await.unwrap();
        assert!(received[0].headers.get("x-goog-api-key").is_none());
    }

    #[tokio::test]
    async fn image_url_download_sends_no_authorization_header() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"data":[{"url": format!("{}/tmp/a.png", server.uri())}]}),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/tmp/a.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(PNG.to_vec()))
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_image_request(&image_request("grok-imagine-image-2.0")).unwrap();
        let images = run_image(&env(&server, &ok, Arc::new(Notify::new())), &plan, "B")
            .await
            .unwrap();
        assert_eq!(images.len(), 1);
        let received = server.received_requests().await.unwrap();
        let download = received
            .iter()
            .find(|r| r.url.path() == "/tmp/a.png")
            .unwrap();
        assert!(
            download.headers.get("authorization").is_none(),
            "不得把 Key 发给图片地址"
        );
    }

    #[tokio::test]
    async fn image_non_image_bytes_and_bad_status_are_stable_errors() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data":[{"b64_json": b64(b"<html>nope</html>")}]})),
            )
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_image_request(&image_request("gpt-image-2")).unwrap();
        let err = run_image(&env(&server, &ok, Arc::new(Notify::new())), &plan, "B")
            .await
            .unwrap_err();
        assert_eq!(err.code, CODE_MEDIA_INVALID);

        for (status, code) in [
            (401, CODE_KEY_INVALID),
            (402, CODE_QUOTA_EXHAUSTED),
            (429, CODE_RATE_LIMITED),
            (400, CODE_REJECTED),
            (500, CODE_UPSTREAM_ERROR),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_string("echo sk-test-secret"))
                .mount(&server)
                .await;
            let err = run_image(&env(&server, &ok, Arc::new(Notify::new())), &plan, "B")
                .await
                .unwrap_err();
            assert_eq!(err.code, code, "status {status}");
            assert!(!err.message.contains("sk-test-secret"), "不得回显 body");
        }
    }

    #[test]
    fn works_survive_reload_and_keep_old_files_without_listing_partial_or_invalid_files() {
        let root = tempfile::tempdir().unwrap();
        assert!(list_studio_works(root.path()).unwrap().is_empty());
        let dir = studio_dir(root.path());
        let media = DecodedMedia {
            bytes: MP4.to_vec(),
            mime: "video/mp4",
        };
        let old = save_media(&dir, "old", 0, &media).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        let new = save_media(&dir, "new", 0, &media).unwrap();
        std::fs::write(dir.join("unfinished.mp4.part"), MP4).unwrap();
        std::fs::write(dir.join("invalid.mp4"), b"not a video").unwrap();
        std::fs::write(dir.join("image.png"), PNG).unwrap();
        let listed = list_studio_works(root.path()).unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(
            listed.iter().filter(|w| w.file.mime == "image/png").count(),
            1
        );
        let first: Vec<_> = listed
            .into_iter()
            .filter(|w| w.file.mime.starts_with("video/"))
            .collect();
        let reloaded: Vec<_> = list_studio_works(root.path())
            .unwrap()
            .into_iter()
            .filter(|w| w.file.mime.starts_with("video/"))
            .collect();
        assert!(old.exists(), "saved works must not expire automatically");
        assert_eq!(first.len(), 2);
        assert_eq!(reloaded.len(), 2);
        assert_eq!(
            first[0].file.path,
            new.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(
            first[1].file.path,
            old.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(first[0].file.mime, "video/mp4");
        assert_eq!(first[0].file.bytes, MP4.len() as u64);
        assert!(first[0].file.data_url.is_none());
        assert!(!new.with_extension("mp4.part").exists());
    }

    #[cfg(unix)]
    #[test]
    fn works_do_not_follow_symlinks_to_files_outside_studio() {
        let root = tempfile::tempdir().unwrap();
        let dir = studio_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        let outside = root.path().join("outside.mp4");
        std::fs::write(&outside, MP4).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("linked.mp4")).unwrap();
        assert!(list_studio_works(root.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn video_download_deadline_ends_a_stalled_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/videos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"stalled"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/stalled"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"succeeded"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/stalled/content"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(2))
                    .set_body_bytes(MP4.to_vec()),
            )
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_video_request(&video_request("grok-imagine-video-1.5")).unwrap();
        let timing = VideoTiming {
            download_timeout: Duration::from_millis(30),
            ..fast()
        };
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            run_video(
                &env(&server, &ok, Arc::new(Notify::new())),
                &plan,
                timing,
                &mut |_| {},
            ),
        )
        .await
        .expect("download deadline must settle the task");
        assert_eq!(result.unwrap_err().code, CODE_MEDIA_TIMEOUT);
    }

    #[tokio::test]
    async fn video_flow_submit_poll_and_download() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/videos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"request_id":"vid-1"})))
            .mount(&server)
            .await;
        // 先返回两次进行中，再成功。
        Mock::given(method("GET"))
            .and(path("/v1/videos/vid-1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"status":"running","progress":30})),
            )
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/vid-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"succeeded"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/vid-1/content"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(MP4.to_vec()))
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_video_request(&video_request("grok-imagine-video-1.5")).unwrap();
        let mut stages: Vec<&'static str> = Vec::new();
        let mut download_progress = Vec::new();
        let mut on_progress = |p: VideoProgress| {
            stages.push(p.stage);
            if p.stage == "downloading" {
                download_progress.push(p.progress);
            }
        };
        let video = run_video(
            &env(&server, &ok, Arc::new(Notify::new())),
            &plan,
            fast(),
            &mut on_progress,
        )
        .await
        .unwrap();
        assert_eq!(video.mime, "video/mp4");
        assert_eq!(stages.first(), Some(&"submitted"));
        assert!(stages.contains(&"running"));
        assert_eq!(download_progress.first(), Some(&None));
        assert!(download_progress.iter().flatten().all(|p| *p < 100));
        assert!(download_progress.iter().flatten().any(|p| *p > 0));
        assert_eq!(stages.last(), Some(&"saving"));
        // 只提交一次：不自动重放。
        let received = server.received_requests().await.unwrap();
        assert_eq!(
            received
                .iter()
                .filter(|r| r.method.as_str() == "POST")
                .count(),
            1
        );
    }

    /// 2026-10-14 实测的 Grok 视频形态：进行中轮询返回 **HTTP 202**
    /// `{"status":"pending","progress":1}`，完成是 **`done`**（不是文档里的
    /// `succeeded`），完成体带 `usage`/`video.url`，成片字节头是 `ftypisom`。
    #[tokio::test]
    async fn video_grok_real_shapes_202_pending_then_done() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let mp4: Vec<u8> = [&[0, 0, 0, 0x20][..], b"ftypisom", &[0u8; 8]].concat();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/videos"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"request_id": "b6f18128-3da6-9092-91c4-9097a4123680"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/b6f18128-3da6-9092-91c4-9097a4123680"))
            .respond_with(
                ResponseTemplate::new(202)
                    .set_body_json(json!({"status": "pending", "progress": 1})),
            )
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/b6f18128-3da6-9092-91c4-9097a4123680"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "grok-imagine-video-1.5",
                "progress": 100,
                "status": "done",
                "usage": {"cost_in_usd_ticks": 4100000000u64},
                "video": {
                    "duration": 5,
                    "respect_moderation": true,
                    "url": "/v1/videos/b6f18128-3da6-9092-91c4-9097a4123680/content"
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/v1/videos/b6f18128-3da6-9092-91c4-9097a4123680/content",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "video/mp4")
                    .set_body_bytes(mp4),
            )
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_video_request(&{
            let mut r = video_request("grok-imagine-video-1.5");
            r.duration = Some(5);
            r.aspect_ratio = Some("16:9".to_string());
            r.resolution = Some("480p".to_string());
            r.reference = Some(reference(PNG, "image/png"));
            r
        })
        .unwrap();
        let mut progress: Vec<Option<u32>> = Vec::new();
        let mut on_progress = |p: VideoProgress| {
            if p.stage == "running" {
                progress.push(p.progress);
            }
        };
        let video = run_video(
            &env(&server, &ok, Arc::new(Notify::new())),
            &plan,
            fast(),
            &mut on_progress,
        )
        .await
        .unwrap();
        assert_eq!(video.mime, "video/mp4");
        assert_eq!(
            progress,
            vec![Some(1), Some(1)],
            "202 的 pending 体要当作进行中"
        );
    }

    /// 实测：Grok 任务失败时轮询仍是 HTTP 200，体里 `status:"failed"` 并带
    /// `error{code,message}`；不透传上游原因，只给稳定 code。
    #[tokio::test]
    async fn video_grok_failed_job_is_http_200_with_error_object() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/videos"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"request_id": "e84c6818"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/e84c6818"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "failed",
                "error": {"code": "invalid_argument", "message": "contains sk-test-secret"}
            })))
            .mount(&server)
            .await;
        let ok = always_ok();
        let plan = validate_video_request(&video_request("grok-imagine-video-1.5")).unwrap();
        let mut noop = |_p: VideoProgress| {};
        let err = run_video(
            &env(&server, &ok, Arc::new(Notify::new())),
            &plan,
            fast(),
            &mut noop,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, CODE_MEDIA_FAILED);
        assert!(!err.message.contains("sk-test-secret"), "不得回显上游信息");
    }

    #[tokio::test]
    async fn video_failed_status_times_out_and_cancels() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let ok = always_ok();
        let plan = validate_video_request(&video_request("doubao-seedance-2.0")).unwrap();

        // 任务失败。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/videos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"t1"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/t1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"failed"})))
            .mount(&server)
            .await;
        let mut noop = |_p: VideoProgress| {};
        let err = run_video(
            &env(&server, &ok, Arc::new(Notify::new())),
            &plan,
            fast(),
            &mut noop,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, CODE_MEDIA_FAILED);

        // 一直进行中 → 超时。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/videos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"t2"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/t2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"running"})))
            .mount(&server)
            .await;
        let timing = VideoTiming {
            poll_interval: Duration::from_millis(10),
            max_wait: Duration::from_millis(80),
            ..VideoTiming::default()
        };
        let err = run_video(
            &env(&server, &ok, Arc::new(Notify::new())),
            &plan,
            timing,
            &mut noop,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, CODE_MEDIA_TIMEOUT);

        // 取消信号。
        let cancel = Arc::new(Notify::new());
        cancel.notify_one();
        let err = run_video(&env(&server, &ok, cancel), &plan, fast(), &mut noop)
            .await
            .unwrap_err();
        assert_eq!(err.code, CODE_CANCELLED);
    }

    #[tokio::test]
    async fn video_download_must_be_a_video_and_identity_change_stops_task() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/videos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"request_id":"v9"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/v9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"succeeded"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/videos/v9/content"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"not a video".to_vec()))
            .mount(&server)
            .await;
        let plan = validate_video_request(&video_request("grok-imagine-video-1.5")).unwrap();
        let mut noop = |_p: VideoProgress| {};
        let ok = always_ok();
        let err = run_video(
            &env(&server, &ok, Arc::new(Notify::new())),
            &plan,
            fast(),
            &mut noop,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, CODE_MEDIA_INVALID);

        // 身份失效：轮询等待期间被 supervised 的 tick 发现。
        let gone = || false;
        let timing = VideoTiming {
            poll_interval: Duration::from_secs(30),
            max_wait: Duration::from_secs(60),
            ..VideoTiming::default()
        };
        let err = tokio::time::timeout(
            Duration::from_secs(10),
            run_video(
                &env(&server, &gone, Arc::new(Notify::new())),
                &plan,
                timing,
                &mut noop,
            ),
        )
        .await
        .expect("应在身份 tick 内终止")
        .unwrap_err();
        assert_eq!(err.code, CODE_IDENTITY_CHANGED);
    }
}
