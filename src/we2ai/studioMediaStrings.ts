/**
 * AI 营销工坊图片 / 视频面板的界面文案（功能 27，P2/P3）。
 *
 * 与 `strings.ts` 分开：媒体面板的文案数量多且只在工坊内使用，单独成文件避免
 * 撑大全局 `We2aiStrings`；错误 code 的本地化也放在这里（新增 code 见 Rust
 * `playground/media.rs`）。
 */

import type { We2aiLanguage } from "./strings";

export interface StudioMediaStrings {
  // 模式切换
  modeText: string;
  modeImage: string;
  modeVideo: string;
  outputLanguageLabel: string;

  // 通用
  modelLabel: string;
  modelPlaceholder: string;
  loadingModels: string;
  templatesLabel: string;
  fieldsLabel: string;
  requiredMark: string;
  promptLabel: string;
  promptEdited: string;
  promptReset: string;
  promptBytes: string;
  promptTooLong: string;
  aiOptimize: string;
  aiOptimizing: string;
  aiOptimizeNeedText: string;
  referenceLabel: string;
  referenceAdd: string;
  referenceRemove: string;
  referenceHintImage: string;
  referenceHintVideo: string;
  referenceHintFirstFrame: string;
  referenceCount: string;
  referenceCountExceeded: string;
  referenceTotalTooLarge: string;
  referencePromptChanged: string;
  referenceMinSide: string;
  referenceUnsupported: string;
  referenceTooLarge: string;
  referenceBadType: string;
  stop: string;
  saveAs: string;
  openFile: string;
  revealFile: string;
  savedTo: string;
  fileActionFailed: string;
  chooseTemplate: string;
  fillRequired: string;

  // 图片
  imageModelsEmpty: string;
  imageSizeLabel: string;
  imageAspectLabel: string;
  imageQualityLabel: string;
  imageCountLabel: string;
  imageGenerate: string;
  imageGenerating: string;
  imageCostNote: string;
  imageResultEmpty: string;

  // 视频
  videoModelsEmpty: string;
  videoDurationLabel: string;
  videoDurationUnit: string;
  videoAspectLabel: string;
  videoResolutionLabel: string;
  videoAudioLabel: string;
  videoGenerate: string;
  videoCostNote: string;
  videoCancelNote: string;
  videoStageSubmitted: string;
  videoStageRunning: string;
  videoStageDownloading: string;
  videoStageSaving: string;
  videoDownloadTimeout: string;
  videoWaitStopped: string;
  worksAll: string;
  worksVideo: string;
  worksImage: string;
  worksFavorites: string;
  worksFavorite: string;
  worksUnfavorite: string;
  worksDelete: string;
  worksDeleteConfirm: string;
  worksCancel: string;
  worksClose: string;
  worksProcessing: string;
  worksFailed: string;
  worksCancelled: string;
  worksInterrupted: string;
  worksSuccess: string;
  worksMore: string;
  worksCompleteNotice: string;
  worksView: string;
  worksPreviewFailed: string;
  worksNoMatch: string;
  worksTitle: string;
  worksDescription: string;
  worksEmpty: string;
  worksLoadError: string;
  worksLoading: string;
  worksDownload: string;
  videoElapsed: string;
  videoResultEmpty: string;
  videoPreviewFailed: string;
  videoShortDurationNote: string;

  // 错误
  errorRejected: string;
  errorMediaFailed: string;
  errorMediaTimeout: string;
  errorMediaInvalid: string;
  errorPathInvalid: string;
  errorSaveFailed: string;
}

const ZH: StudioMediaStrings = {
  modeText: "文本",
  modeImage: "图片",
  modeVideo: "视频",
  outputLanguageLabel: "输出语言",

  modelLabel: "模型",
  modelPlaceholder: "选择模型",
  loadingModels: "正在加载模型…",
  templatesLabel: "场景模板",
  fieldsLabel: "产品信息",
  requiredMark: "必填",
  promptLabel: "提示词（可直接修改）",
  promptEdited: "已手动修改，更换模板或产品信息不会覆盖",
  promptReset: "恢复模板提示词",
  promptBytes: "{used} / {max} 字节",
  promptTooLong: "提示词超出该模型上限，请精简",
  aiOptimize: "AI 优化提示词",
  aiOptimizing: "优化中…",
  aiOptimizeNeedText: "AI 优化需要该 Key 下有可用的文本模型",
  referenceLabel: "参考图（可选）",
  referenceAdd: "上传产品图",
  referenceRemove: "移除",
  referenceHintImage: "上传后按这张图保持设备外形，只换背景和光线",
  referenceHintVideo:
    "可多选或继续添加，按编号引用不同角度、产品和场景，不固定为首帧；PNG/JPEG/WebP，单张 ≤ 8 MB，合计 ≤ 32 MB；含真人面孔的图片可能被网关拒绝",
  referenceHintFirstFrame: "该模型仅支持一张首帧图；PNG/JPEG/WebP，单张 ≤ 8 MB",
  referenceCount: "已选 {used} / {max} 张参考图",
  referenceCountExceeded: "该模型最多支持 {max} 张参考图，请移除多余图片后生成",
  referenceTotalTooLarge: "参考图总大小不能超过 32 MB，请压缩或移除部分图片",
  referencePromptChanged:
    "参考图已变更，请同步检查已编辑提示词中的图片编号；可恢复模板提示词自动更新",
  referenceMinSide: "宽高都不能小于 {n} 像素",
  referenceUnsupported: "该模型不支持参考图",
  referenceTooLarge: "图片超过 8 MB，请压缩后再上传",
  referenceBadType: "仅支持 PNG、JPEG、WebP",
  stop: "停止",
  saveAs: "另存为",
  openFile: "打开",
  revealFile: "在文件夹中显示",
  savedTo: "已保存到 {path}",
  fileActionFailed: "无法操作该文件",
  chooseTemplate: "先选择一个场景模板",
  fillRequired: "请填写必填项：{fields}",

  imageModelsEmpty: "该 Key 暂无可用的图片模型",
  imageSizeLabel: "尺寸",
  imageAspectLabel: "画幅",
  imageQualityLabel: "分辨率",
  imageCountLabel: "张数",
  imageGenerate: "生成图片",
  imageGenerating: "生成中，通常需要 1 到 2 分钟…",
  imageCostNote: "按张计费，分辨率越高越贵；以账单为准",
  imageResultEmpty: "生成的图片会显示在这里，并保存到本机",

  videoModelsEmpty: "该 Key 暂无可用的视频模型",
  videoDurationLabel: "时长",
  videoDurationUnit: "{n} 秒",
  videoAspectLabel: "画幅",
  videoResolutionLabel: "分辨率",
  videoAudioLabel: "同步生成音频",
  videoGenerate: "生成视频",
  videoCostNote: "按分辨率和时长计费，生成成功才扣费；以账单为准",
  videoCancelNote: "停止只会放弃等待，已提交的任务可能仍在上游继续并计费",
  videoStageSubmitted: "已提交，排队中",
  videoStageRunning: "生成中",
  videoStageDownloading: "下载视频中",
  videoStageSaving: "保存视频中",
  videoDownloadTimeout:
    "下载等待超时，请先到作品列表查看是否已保存，再决定后续操作",
  videoWaitStopped: "已停止等待，已保存的视频可在作品列表查看",
  worksAll: "全部",
  worksVideo: "视频",
  worksImage: "图片",
  worksFavorites: "仅收藏",
  worksFavorite: "收藏",
  worksUnfavorite: "取消收藏",
  worksDelete: "删除作品",
  worksDeleteConfirm: "删除后无法恢复，将删除本机作品及其文件。确认继续吗？",
  worksCancel: "取消",
  worksClose: "关闭",
  worksProcessing: "生成中，可切换其他功能，完成后会提醒。",
  worksFailed: "生成失败",
  worksCancelled: "已停止等待，上游任务可能仍继续计费",
  worksInterrupted: "本机等待已结束，上游任务可能仍在运行；请勿直接重复提交。",
  worksSuccess: "已完成",
  worksMore: "加载更多",
  worksCompleteNotice: "作品已生成并保存，可前往作品列表查看。",
  worksView: "查看作品列表",
  worksPreviewFailed: "无法预览，可下载或使用系统程序打开。",
  worksNoMatch: "当前筛选下没有作品",
  worksTitle: "作品列表",
  worksDescription: "统一管理本机的图片和视频，查看生成状态、收藏与下载。",
  worksEmpty: "还没有作品，去 AI 营销工坊开始创作。",
  worksLoadError: "读取本机作品失败，请刷新重试",
  worksLoading: "正在读取作品…",
  worksDownload: "下载 / 另存为",
  videoElapsed: "已用时 {secs} 秒",
  videoResultEmpty: "生成的视频会显示在这里，并保存到本机，通常需要几分钟",
  videoPreviewFailed: "无法在应用内预览，请用系统播放器打开",
  videoShortDurationNote: "不足 10 秒时会压缩为三镜头",

  errorRejected:
    "请求被网关拒绝（参数不受支持或内容未通过审核），请调整提示词或参数",
  errorMediaFailed: "生成失败（可能是提示词或参考图未通过审核），请调整后重试",
  errorMediaTimeout: "生成超时，请稍后重试；若已扣费请联系客服",
  errorMediaInvalid: "网关返回的内容不是有效的图片或视频，请稍后重试",
  errorPathInvalid: "文件不存在或不在工坊目录内",
  errorSaveFailed: "保存失败，请检查磁盘空间和目录权限",
};

const EN: StudioMediaStrings = {
  modeText: "Text",
  modeImage: "Image",
  modeVideo: "Video",
  outputLanguageLabel: "Output language",

  modelLabel: "Model",
  modelPlaceholder: "Select a model",
  loadingModels: "Loading models…",
  templatesLabel: "Templates",
  fieldsLabel: "Product details",
  requiredMark: "required",
  promptLabel: "Prompt (editable)",
  promptEdited:
    "Edited by hand; changing the template or details will not overwrite it",
  promptReset: "Restore template prompt",
  promptBytes: "{used} / {max} bytes",
  promptTooLong: "Prompt exceeds this model's limit; please shorten it",
  aiOptimize: "Refine with AI",
  aiOptimizing: "Refining…",
  aiOptimizeNeedText: "Refining needs a text model available under this key",
  referenceLabel: "Reference image (optional)",
  referenceAdd: "Upload product photo",
  referenceRemove: "Remove",
  referenceHintImage:
    "Keeps the machine's shape from this photo and changes only background and lighting",
  referenceHintVideo:
    "Select multiple photos or add more. Refer to numbered angles, products and scenes without pinning the first frame. PNG/JPEG/WebP; up to 8 MB each, 32 MB total. Real faces may be rejected.",
  referenceHintFirstFrame:
    "This model supports one first-frame image. PNG/JPEG/WebP; up to 8 MB.",
  referenceCount: "{used} / {max} reference images selected",
  referenceCountExceeded:
    "This model supports up to {max} references. Remove extra images before generating.",
  referenceTotalTooLarge:
    "References exceed 32 MB in total. Compress or remove some images.",
  referencePromptChanged:
    "References changed. Check image numbers in your edited prompt, or restore the template to update them automatically.",
  referenceMinSide: "Width and height must both be at least {n} px",
  referenceUnsupported: "This model does not support a reference image",
  referenceTooLarge: "Image is over 8 MB; please compress it first",
  referenceBadType: "Only PNG, JPEG and WebP are supported",
  stop: "Stop",
  saveAs: "Save as",
  openFile: "Open",
  revealFile: "Show in folder",
  savedTo: "Saved to {path}",
  fileActionFailed: "Could not act on this file",
  chooseTemplate: "Pick a template first",
  fillRequired: "Please fill in: {fields}",

  imageModelsEmpty: "No image models available for this key",
  imageSizeLabel: "Size",
  imageAspectLabel: "Aspect ratio",
  imageQualityLabel: "Resolution",
  imageCountLabel: "Images",
  imageGenerate: "Generate image",
  imageGenerating: "Generating, usually 1 to 2 minutes…",
  imageCostNote:
    "Billed per image; higher resolution costs more. See your bill for exact amounts",
  imageResultEmpty:
    "Generated images appear here and are saved on this computer",

  videoModelsEmpty: "No video models available for this key",
  videoDurationLabel: "Duration",
  videoDurationUnit: "{n} s",
  videoAspectLabel: "Aspect ratio",
  videoResolutionLabel: "Resolution",
  videoAudioLabel: "Generate audio",
  videoGenerate: "Generate video",
  videoCostNote:
    "Billed by resolution and duration, only when generation succeeds. See your bill for exact amounts",
  videoCancelNote:
    "Stopping only abandons the wait; a submitted job may keep running upstream and still be billed",
  videoStageSubmitted: "Submitted, queued",
  videoStageRunning: "Generating",
  videoStageDownloading: "Downloading video",
  videoStageSaving: "Saving video",
  videoDownloadTimeout:
    "Download wait timed out. Check Works for a saved video before taking further action.",
  videoWaitStopped: "Stopped waiting. Saved videos remain available in Works.",
  worksAll: "All",
  worksVideo: "Video",
  worksImage: "Image",
  worksFavorites: "Favorites",
  worksFavorite: "Favorite",
  worksUnfavorite: "Unfavorite",
  worksDelete: "Delete work",
  worksDeleteConfirm:
    "This permanently deletes the local work and its files. Continue?",
  worksCancel: "Cancel",
  worksClose: "Close",
  worksProcessing:
    "Generating. You can switch features and will be notified when done.",
  worksFailed: "Generation failed",
  worksCancelled: "Waiting stopped. The upstream task may still incur charges",
  worksInterrupted:
    "Local waiting ended. The upstream task may still be running; avoid resubmitting.",
  worksSuccess: "Completed",
  worksMore: "Load more",
  worksCompleteNotice:
    "Your work is ready and saved. View it in the works list.",
  worksView: "View works list",
  worksPreviewFailed:
    "Preview unavailable. Download or open with the system app.",
  worksNoMatch: "No works match this filter",
  worksTitle: "Works list",
  worksDescription:
    "Manage local images and videos, track generation, favorite and download.",
  worksEmpty: "No works yet. Start creating in the AI marketing studio.",
  worksLoadError: "Could not load saved works. Please refresh and try again.",
  worksLoading: "Loading saved works…",
  worksDownload: "Download / Save as",
  videoElapsed: "{secs} s elapsed",
  videoResultEmpty:
    "The generated video appears here and is saved on this computer; it usually takes a few minutes",
  videoPreviewFailed:
    "Cannot preview in the app; open it with the system player",
  videoShortDurationNote:
    "Under 10 seconds the storyboard is compressed to three shots",

  errorRejected:
    "The gateway rejected the request (unsupported parameters or content review); adjust the prompt or settings",
  errorMediaFailed:
    "Generation failed (the prompt or reference image may not have passed review); adjust and retry",
  errorMediaTimeout:
    "Generation timed out; try again later, and contact support if you were charged",
  errorMediaInvalid:
    "The gateway returned something that is not a valid image or video; try again later",
  errorPathInvalid: "The file does not exist or is outside the studio folder",
  errorSaveFailed: "Save failed; check disk space and folder permissions",
};

export function getStudioMediaStrings(
  language: We2aiLanguage,
): StudioMediaStrings {
  return language === "zh" ? ZH : EN;
}
