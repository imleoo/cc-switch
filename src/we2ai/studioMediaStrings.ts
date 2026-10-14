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
  referenceHintVideo: "作为视频首帧；含真人面孔的图片可能被网关拒绝",
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
    "Used as the first frame; images with real faces may be rejected by the gateway",
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
