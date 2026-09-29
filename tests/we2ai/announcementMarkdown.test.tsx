import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { settingsApi } from "@/lib/api/settings";
import { AnnouncementContent } from "@/we2ai/AnnouncementDialog";
import { renderAnnouncementHtml } from "@/we2ai/announcementMarkdown";

function parse(html: string): HTMLDivElement {
  const div = document.createElement("div");
  div.innerHTML = html;
  return div;
}

describe("renderAnnouncementHtml sanitization", () => {
  it("removes script elements together with their content", () => {
    const html = renderAnnouncementHtml("你好<script>alert(1)</script>世界");
    expect(html).not.toContain("script");
    expect(html).not.toContain("alert");
    expect(parse(html).textContent).toContain("你好");
  });

  it("strips inline event handlers such as onerror", () => {
    const html = renderAnnouncementHtml('<img src=x onerror="alert(1)">文字');
    const root = parse(html);
    expect(root.querySelector("img")).toBeNull();
    expect(html).not.toMatch(/onerror/i);
  });

  it("strips on* attributes even from allowed tags", () => {
    const html = renderAnnouncementHtml(
      '<p onclick="alert(1)" onmouseover="x()">段落</p>',
    );
    expect(html).not.toMatch(/onclick|onmouseover/i);
    expect(parse(html).querySelector("p")?.textContent).toBe("段落");
  });

  it("drops the href of javascript: links but keeps the text", () => {
    const html = renderAnnouncementHtml("[点我](javascript:alert(1))");
    const a = parse(html).querySelector("a");
    expect(html).not.toMatch(/javascript:/i);
    expect(a).not.toBeNull();
    expect(a!.hasAttribute("href")).toBe(false);
    expect(a!.textContent).toBe("点我");
  });

  it("drops javascript: hrefs written as raw HTML, including obfuscated ones", () => {
    for (const raw of [
      '<a href="javascript:alert(1)">x</a>',
      '<a href="  JaVaScRiPt:alert(1)">x</a>',
      '<a href="java&#x09;script:alert(1)">x</a>',
    ]) {
      const a = parse(renderAnnouncementHtml(raw)).querySelector("a");
      expect(a?.hasAttribute("href") ?? false).toBe(false);
    }
  });

  it("removes data: images and data: links", () => {
    const md =
      '![a](data:image/png;base64,AAAA)\n\n<a href="data:text/html,<b>x</b>">d</a>';
    const html = renderAnnouncementHtml(md);
    expect(html).not.toContain("data:");
    expect(parse(html).querySelector("img")).toBeNull();
  });

  it("removes iframe, style, form, object and embed elements", () => {
    const html = renderAnnouncementHtml(
      '<iframe src="https://evil.example"></iframe>' +
        "<style>body{display:none}</style>" +
        '<form action="https://evil.example"><input name="x"><button>go</button></form>' +
        '<object data="x"></object><embed src="x">' +
        "正文",
    );
    const root = parse(html);
    for (const tag of [
      "iframe",
      "style",
      "form",
      "input",
      "button",
      "object",
      "embed",
    ]) {
      expect(root.querySelector(tag), tag).toBeNull();
    }
    expect(root.textContent).toContain("正文");
  });

  it("only keeps http and https hrefs", () => {
    const html = renderAnnouncementHtml(
      [
        "[a](https://we2ai.com/x)",
        "[b](http://we2ai.com/y)",
        "[c](file:///etc/passwd)",
        "[d](/relative)",
        "[e](mailto:a@b.c)",
        "[f](vbscript:x)",
      ].join("\n\n"),
    );
    const hrefs = Array.from(parse(html).querySelectorAll("a")).map((a) =>
      a.getAttribute("href"),
    );
    expect(hrefs).toEqual([
      "https://we2ai.com/x",
      "http://we2ai.com/y",
      null,
      null,
      null,
      null,
    ]);
  });

  it("removes attributes other than href (target, style, class, id, data-*)", () => {
    const html = renderAnnouncementHtml(
      '<a href="https://we2ai.com" target="_blank" style="color:red" class="x" id="y" data-k="v">l</a>',
    );
    const a = parse(html).querySelector("a")!;
    expect(a.getAttributeNames()).toEqual(["href"]);
  });

  it("renders regular markdown into the whitelisted tags", () => {
    const md = [
      "# 大标题",
      "",
      "段落 **加粗** 与 *斜体* 和 `代码`",
      "",
      "- 项一",
      "- 项二",
      "",
      "1. 第一",
      "2. 第二",
      "",
      "> 引用",
      "",
      "```",
      "pre block",
      "```",
      "",
      "---",
      "",
      "[官网](https://we2ai.com)",
    ].join("\n");
    const root = parse(renderAnnouncementHtml(md));
    expect(root.querySelector("h1")?.textContent).toBe("大标题");
    expect(root.querySelector("strong")?.textContent).toBe("加粗");
    expect(root.querySelector("em")?.textContent).toBe("斜体");
    expect(root.querySelector("p code")?.textContent).toBe("代码");
    expect(root.querySelectorAll("ul li")).toHaveLength(2);
    expect(root.querySelectorAll("ol li")).toHaveLength(2);
    expect(root.querySelector("blockquote")?.textContent).toContain("引用");
    expect(root.querySelector("pre code")?.textContent).toContain("pre block");
    expect(root.querySelector("hr")).not.toBeNull();
    expect(root.querySelector("a")?.getAttribute("href")).toBe(
      "https://we2ai.com",
    );
  });

  it("turns single newlines into <br>", () => {
    const root = parse(renderAnnouncementHtml("第一行\n第二行"));
    expect(root.querySelector("br")).not.toBeNull();
  });

  it("returns an empty string for empty input", () => {
    expect(renderAnnouncementHtml("")).toBe("");
  });
});

describe("AnnouncementContent link handling", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("opens http(s) links through openExternal and prevents in-app navigation", () => {
    const openExternal = vi
      .spyOn(settingsApi, "openExternal")
      .mockResolvedValue();
    render(<AnnouncementContent markdown="[官网](https://we2ai.com/docs)" />);

    const notPrevented = fireEvent.click(
      screen.getByRole("link", { name: "官网" }),
    );

    expect(notPrevented).toBe(false);
    expect(openExternal).toHaveBeenCalledTimes(1);
    expect(openExternal).toHaveBeenCalledWith("https://we2ai.com/docs");
  });

  it("swallows clicks on links without a safe href and never opens anything", () => {
    const openExternal = vi
      .spyOn(settingsApi, "openExternal")
      .mockResolvedValue();
    render(<AnnouncementContent markdown="[坏链接](javascript:alert(1))" />);

    const target = screen.getByText("坏链接");
    const notPrevented = fireEvent.click(target);

    expect(notPrevented).toBe(false);
    expect(openExternal).not.toHaveBeenCalled();
  });

  it("does not throw or navigate when openExternal rejects", async () => {
    const openExternal = vi
      .spyOn(settingsApi, "openExternal")
      .mockRejectedValue(new Error("boom"));
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    render(<AnnouncementContent markdown="[官网](https://we2ai.com)" />);

    const notPrevented = fireEvent.click(
      screen.getByRole("link", { name: "官网" }),
    );
    await waitFor(() => expect(debug).toHaveBeenCalled());

    expect(notPrevented).toBe(false);
    expect(openExternal).toHaveBeenCalledTimes(1);
  });

  it("opens the link when the click lands on an element nested inside the anchor", () => {
    const openExternal = vi
      .spyOn(settingsApi, "openExternal")
      .mockResolvedValue();
    render(
      <AnnouncementContent markdown="[**加粗链接**](https://we2ai.com/n)" />,
    );

    const notPrevented = fireEvent.click(screen.getByText("加粗链接"));

    expect(notPrevented).toBe(false);
    expect(openExternal).toHaveBeenCalledWith("https://we2ai.com/n");
  });

  it("handles Ctrl/Cmd clicks the same way: system browser only", () => {
    const openExternal = vi
      .spyOn(settingsApi, "openExternal")
      .mockResolvedValue();
    render(<AnnouncementContent markdown="[官网](https://we2ai.com)" />);
    const link = screen.getByRole("link", { name: "官网" });

    expect(fireEvent.click(link, { ctrlKey: true })).toBe(false);
    expect(fireEvent.click(link, { metaKey: true })).toBe(false);
    expect(openExternal).toHaveBeenCalledTimes(2);
  });

  it("prevents in-app navigation for middle-click (auxclick)", () => {
    const openExternal = vi
      .spyOn(settingsApi, "openExternal")
      .mockResolvedValue();
    render(<AnnouncementContent markdown="[官网](https://we2ai.com)" />);

    const notPrevented = fireEvent(
      screen.getByRole("link", { name: "官网" }),
      new MouseEvent("auxclick", {
        bubbles: true,
        cancelable: true,
        button: 1,
      }),
    );

    expect(notPrevented).toBe(false);
    expect(openExternal).not.toHaveBeenCalled();
  });

  it("opens the link when activated from the keyboard (Enter)", async () => {
    const openExternal = vi
      .spyOn(settingsApi, "openExternal")
      .mockResolvedValue();
    render(<AnnouncementContent markdown="[官网](https://we2ai.com/k)" />);

    screen.getByRole("link", { name: "官网" }).focus();
    await userEvent.keyboard("{Enter}");

    expect(openExternal).toHaveBeenCalledWith("https://we2ai.com/k");
  });
});
