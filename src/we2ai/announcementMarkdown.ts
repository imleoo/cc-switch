import DOMPurify from "dompurify";
import { marked } from "marked";

/**
 * 公告正文（Markdown）渲染：`marked` 转 HTML，再经 `DOMPurify` 按严格白名单净化。
 *
 * 白名单只保留文本排版类标签，img / style / script / iframe / form 以及所有
 * `on*` 事件属性一律剔除（属性白名单仅 `href`）；`href` 只允许 http / https，
 * 其余协议（javascript: / data: / file: / 相对路径…）会被去掉 `href`。
 * 已知取舍：GFM 表格、图片不在白名单内，渲染时只保留其中的文字。
 */
export const ANNOUNCEMENT_ALLOWED_TAGS: readonly string[] = [
  "p",
  "br",
  "strong",
  "em",
  "ul",
  "ol",
  "li",
  "code",
  "pre",
  "blockquote",
  "h1",
  "h2",
  "h3",
  "h4",
  "a",
  "hr",
];

const HTTP_URL = /^https?:\/\//i;

export function renderAnnouncementHtml(markdown: string): string {
  const raw = marked.parse(markdown ?? "", {
    async: false,
    gfm: true,
    breaks: true,
  });
  return DOMPurify.sanitize(raw, {
    ALLOWED_TAGS: [...ANNOUNCEMENT_ALLOWED_TAGS],
    ALLOWED_ATTR: ["href"],
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: false,
    ALLOWED_URI_REGEXP: HTTP_URL,
  });
}

/** 只有 http / https 链接才允许交给系统浏览器打开。 */
export function isOpenableAnnouncementUrl(href: string | null): href is string {
  return typeof href === "string" && HTTP_URL.test(href);
}
