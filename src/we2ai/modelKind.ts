import type { We2aiModelView } from "./api";

/**
 * 模型广场与代码示例只展示文本模型：B1 `kind` 缺失、为空或等于 `text` 时显示，
 * 其余（image/video/audio/other 以及未来未知值）一律隐藏。缺失 `kind` 对应不返回
 * 该字段的旧服务端，不做过滤。类型由服务端判定，客户端不按名称猜测。
 */
export function isTextModel(model: Pick<We2aiModelView, "kind">): boolean {
  return !model.kind || model.kind === "text";
}
