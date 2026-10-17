import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { we2aiApi, type We2aiStudioWork } from "@/we2ai/api";
import { WorksPage } from "@/we2ai/WorksPage";
import { getWe2aiStrings } from "@/we2ai/strings";
import { getStudioMediaStrings } from "@/we2ai/studioMediaStrings";

const t = getWe2aiStrings("zh");
const ms = getStudioMediaStrings("zh");
const work: We2aiStudioWork = {
  id: "saved-video",
  kind: "video",
  status: "success",
  favorite: false,
  model: "grok",
  progress: 100,
  message: null,
  content: null,
  name: "saved-video.mp4",
  savedAtMs: 1792162964000,
  files: [
    {
      path: "/home/u/.we2ai/studio/saved-video.mp4",
      mime: "video/mp4",
      bytes: 8_000_000,
      dataUrl: null,
    },
  ],
};

describe("WorksPage", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.spyOn(we2aiApi, "playgroundMediaList").mockResolvedValue([work]);
  });

  it("重新挂载仍读取持久化作品，可预览、下载与打开文件位置", async () => {
    const save = vi
      .spyOn(we2aiApi, "playgroundMediaSaveAs")
      .mockResolvedValue("/downloads/video.mp4");
    const open = vi
      .spyOn(we2aiApi, "playgroundMediaOpen")
      .mockResolvedValue(undefined);
    const view = render(<WorksPage t={t} language="zh" refreshTick={0} />);
    expect(await screen.findByTestId("we2ai-work-player")).toHaveAttribute(
      "src",
      `asset://localhost${work.files[0].path}`,
    );
    await userEvent.click(
      screen.getByRole("button", { name: ms.worksDownload }),
    );
    expect(save).toHaveBeenCalledWith(work.files[0].path);
    expect(await screen.findByRole("status")).toHaveTextContent(
      "/downloads/video.mp4",
    );
    await userEvent.click(screen.getByRole("button", { name: ms.openFile }));
    expect(open).toHaveBeenLastCalledWith(work.files[0].path, false);
    await userEvent.click(screen.getByRole("button", { name: ms.revealFile }));
    expect(open).toHaveBeenLastCalledWith(work.files[0].path, true);
    view.unmount();
    render(<WorksPage t={t} language="zh" refreshTick={0} />);
    expect(await screen.findByTestId("we2ai-work-player")).toBeInTheDocument();
    expect(we2aiApi.playgroundMediaList).toHaveBeenCalledTimes(2);
  });

  it.each(["video", "image"] as const)(
    "%s 详情可用顶部或底部关闭按钮关闭，关闭后可重新打开",
    async (kind) => {
      vi.mocked(we2aiApi.playgroundMediaList).mockResolvedValue([
        { ...work, kind },
      ]);
      render(<WorksPage t={t} language="zh" refreshTick={0} />);
      const title = await screen.findByRole("button", { name: work.name });
      for (const index of [0, 1]) {
        await userEvent.click(title);
        const dialog = screen.getByRole("dialog");
        const buttons = within(dialog).getAllByRole("button", { name: "关闭" });
        expect(buttons).toHaveLength(2);
        await userEvent.click(buttons[index]);
        await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      }
      expect(we2aiApi.playgroundMediaList).toHaveBeenCalledTimes(1);
      expect(screen.getByTestId("we2ai-work")).toBeInTheDocument();
    },
  );

  it("详情点击内容保持打开，点击遮罩或按 Esc 都可关闭", async () => {
    render(<WorksPage t={t} language="zh" refreshTick={0} />);
    const title = await screen.findByRole("button", { name: work.name });
    await userEvent.click(title);
    const dialog = screen.getByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("heading", { name: work.name }),
    );
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    // The portal puts the overlay immediately before its content.
    const overlay = dialog.previousElementSibling!;
    fireEvent.pointerDown(overlay, {
      button: 0,
      pointerType: "mouse",
      ctrlKey: false,
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await userEvent.click(title);
    await userEvent.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("英文详情同样提供可点击的关闭入口", async () => {
    render(
      <WorksPage t={getWe2aiStrings("en")} language="en" refreshTick={0} />,
    );
    await userEvent.click(
      await screen.findByRole("button", { name: work.name }),
    );
    const buttons = within(screen.getByRole("dialog")).getAllByRole("button", {
      name: "Close",
    });
    expect(buttons).toHaveLength(2);
    await userEvent.click(buttons[0]);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("空列表显示提示，生成终态触发刷新后新视频出现", async () => {
    vi.mocked(we2aiApi.playgroundMediaList).mockResolvedValueOnce([]);
    const view = render(<WorksPage t={t} language="zh" refreshTick={0} />);
    expect(await screen.findByText(ms.worksEmpty)).toBeInTheDocument();
    view.rerender(<WorksPage t={t} language="zh" refreshTick={1} />);
    expect(await screen.findByTestId("we2ai-work-player")).toBeInTheDocument();
    expect(screen.queryByText(ms.worksEmpty)).toBeNull();
  });

  it("读取失败可手动重试，预览失败仍保留下载与打开入口", async () => {
    vi.mocked(we2aiApi.playgroundMediaList).mockRejectedValueOnce(
      new Error("disk error"),
    );
    render(<WorksPage t={t} language="zh" refreshTick={0} />);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      ms.worksLoadError,
    );
    await userEvent.click(screen.getByTestId("we2ai-works-refresh"));
    const player = await screen.findByTestId("we2ai-work-player");
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    fireEvent.error(player);
    expect(screen.getByText(ms.worksPreviewFailed)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: ms.worksDownload }),
    ).toBeEnabled();
    expect(screen.getByRole("button", { name: ms.openFile })).toBeEnabled();
  });
  it("按类型和收藏筛选，收藏持久化，删除需要明确确认", async () => {
    const image: We2aiStudioWork = {
      ...work,
      id: "image",
      name: "产品图",
      kind: "image",
      files: [
        { ...work.files[0], path: "/h/.we2ai/studio/a.png", mime: "image/png" },
      ],
    };
    let records = [work, image];
    vi.mocked(we2aiApi.playgroundMediaList).mockImplementation(async () =>
      records.map((w) => ({ ...w })),
    );
    const update = vi
      .spyOn(we2aiApi, "playgroundWorkUpdate")
      .mockImplementation(async (id, favorite) => {
        records =
          favorite === null
            ? records.filter((w) => w.id !== id)
            : records.map((w) => (w.id === id ? { ...w, favorite } : w));
      });
    render(<WorksPage t={t} language="zh" refreshTick={0} />);
    expect(await screen.findAllByTestId("we2ai-work")).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: ms.worksImage }));
    expect(screen.getAllByTestId("we2ai-work")).toHaveLength(1);
    await userEvent.click(
      screen.getByRole("button", { name: ms.worksFavorite }),
    );
    expect(
      await screen.findByRole("button", { name: ms.worksUnfavorite }),
    ).toBeInTheDocument();
    expect(update).toHaveBeenCalledWith(image.id, true);
    await userEvent.click(screen.getByRole("button", { name: ms.worksAll }));
    await userEvent.click(
      screen.getByRole("button", { name: ms.worksFavorites }),
    );
    expect(screen.getAllByTestId("we2ai-work")).toHaveLength(1);
    await userEvent.click(screen.getByRole("button", { name: ms.worksDelete }));
    let dialog = screen.getByRole("dialog");
    fireEvent.pointerDown(dialog.previousElementSibling!, {
      button: 0,
      pointerType: "mouse",
      ctrlKey: false,
    });
    expect(screen.getByRole("dialog")).toBe(dialog);
    await userEvent.click(
      within(dialog).getByRole("button", { name: ms.worksCancel }),
    );
    expect(update).toHaveBeenCalledTimes(1);
    await userEvent.click(screen.getByRole("button", { name: ms.worksDelete }));
    dialog = screen.getByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: ms.worksDelete }),
    );
    await waitFor(() =>
      expect(screen.queryAllByTestId("we2ai-work")).toHaveLength(0),
    );
    expect(update).toHaveBeenLastCalledWith(image.id, null);
  });

  it("只展示图片和视频，隐藏历史文案及复制入口，多图仍可分别下载", async () => {
    const text: We2aiStudioWork = {
      ...work,
      id: "text",
      name: "产品文案",
      kind: "text",
      favorite: true,
      content: "完整营销文案",
      files: [
        {
          ...work.files[0],
          path: "/h/.we2ai/studio/a.txt",
          mime: "text/plain",
        },
      ],
    };
    const image: We2aiStudioWork = {
      ...work,
      id: "images",
      name: "两张产品图",
      kind: "image",
      files: ["a", "b"].map((id) => ({
        ...work.files[0],
        path: `/h/.we2ai/studio/${id}.png`,
        mime: "image/png",
      })),
    };
    vi.mocked(we2aiApi.playgroundMediaList).mockResolvedValue([
      text,
      image,
      work,
    ]);
    const update = vi
      .spyOn(we2aiApi, "playgroundWorkUpdate")
      .mockResolvedValue(undefined);
    render(<WorksPage t={t} language="zh" refreshTick={0} />);
    expect(await screen.findAllByTestId("we2ai-work")).toHaveLength(2);
    expect(screen.queryByText("产品文案")).toBeNull();
    expect(screen.queryByText("完整营销文案")).toBeNull();
    expect(screen.queryByRole("button", { name: "文案" })).toBeNull();
    expect(screen.queryByRole("button", { name: "复制文案" })).toBeNull();
    expect(screen.getByText(ms.worksDescription)).not.toHaveTextContent("文案");
    expect(update).not.toHaveBeenCalled();
    await userEvent.click(
      screen.getByRole("button", { name: ms.worksFavorites }),
    );
    expect(await screen.findByText(ms.worksNoMatch)).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: ms.worksFavorites }),
    );
    await userEvent.click(screen.getByRole("button", { name: image.name }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getAllByTestId("we2ai-work-image")).toHaveLength(2);
    expect(
      within(dialog).getAllByRole("button", { name: /下载/ }),
    ).toHaveLength(2);
  });

  it("只有历史文案时显示空列表，刷新也不会恢复文案入口", async () => {
    vi.mocked(we2aiApi.playgroundMediaList).mockResolvedValue([
      {
        ...work,
        kind: "text",
        name: "旧文案",
        content: "旧文案内容",
        status: "processing",
        files: [],
      },
    ]);
    render(<WorksPage t={t} language="zh" refreshTick={0} />);
    expect(await screen.findByText(ms.worksEmpty)).toBeInTheDocument();
    await userEvent.click(screen.getByTestId("we2ai-works-refresh"));
    await waitFor(() =>
      expect(we2aiApi.playgroundMediaList).toHaveBeenCalledTimes(2),
    );
    expect(screen.queryByText("旧文案")).toBeNull();
    expect(screen.queryAllByTestId("we2ai-work")).toHaveLength(0);
  });

  it("列表分批展示，生成中不提供删除操作", async () => {
    vi.mocked(we2aiApi.playgroundMediaList).mockResolvedValue(
      Array.from({ length: 21 }, (_, i) => ({
        ...work,
        id: String(i),
        name: `作品${i}`,
        status: "processing",
        files: [],
        progress: 22,
      })),
    );
    render(<WorksPage t={t} language="zh" refreshTick={0} />);
    expect(await screen.findAllByTestId("we2ai-work")).toHaveLength(20);
    expect(screen.queryByRole("button", { name: ms.worksDelete })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: ms.worksMore }));
    expect(screen.getAllByTestId("we2ai-work")).toHaveLength(21);
  });
});
