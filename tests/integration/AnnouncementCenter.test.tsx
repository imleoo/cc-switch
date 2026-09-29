import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  we2aiApi,
  type We2aiAnnouncement,
  type We2aiApiError,
} from "@/we2ai/api";
import { AnnouncementCenter } from "@/we2ai/AnnouncementCenter";
import { getWe2aiStrings } from "@/we2ai/strings";

const t = getWe2aiStrings("zh");

function ann(
  id: number,
  overrides: Partial<We2aiAnnouncement> = {},
): We2aiAnnouncement {
  return {
    id,
    title: `公告${id}`,
    content: `正文 **${id}**`,
    notifyMode: "popup",
    startsAt: null,
    endsAt: null,
    readAt: null,
    createdAt: `2026-01-0${id}T00:00:00Z`,
    ...overrides,
  };
}

let changedHandler: (() => void) | null = null;

function mockApi(
  list: We2aiAnnouncement[] | (() => Promise<We2aiAnnouncement[]>),
) {
  const listAnnouncements = vi
    .spyOn(we2aiApi, "listAnnouncements")
    .mockImplementation(typeof list === "function" ? list : async () => list);
  const markRead = vi
    .spyOn(we2aiApi, "markAnnouncementRead")
    .mockResolvedValue();
  return { listAnnouncements, markRead };
}

describe("AnnouncementCenter", () => {
  beforeEach(() => {
    changedHandler = null;
    vi.spyOn(we2aiApi, "onAnnouncementsChanged").mockImplementation(
      async (handler) => {
        changedHandler = handler;
        return () => {
          changedHandler = null;
        };
      },
    );
    vi.spyOn(console, "debug").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("queues popup announcements oldest first and marks each one read when dismissed", async () => {
    const { markRead } = mockApi([
      ann(2), // 较新
      ann(1), // 较旧
      ann(3, { notifyMode: "silent" }),
    ]);
    render(<AnnouncementCenter t={t} />);

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("公告1")).toBeInTheDocument();
    expect(within(dialog).getByText("还有 1 条未读公告")).toBeInTheDocument();
    expect(markRead).not.toHaveBeenCalled();

    await userEvent.click(
      within(dialog).getByRole("button", { name: t.announcementGotIt }),
    );
    expect(markRead).toHaveBeenCalledWith(1);

    await waitFor(() =>
      expect(
        within(screen.getByRole("dialog")).getByText("公告2"),
      ).toBeInTheDocument(),
    );
    expect(screen.queryByText("还有 1 条未读公告")).not.toBeInTheDocument();

    await userEvent.click(
      screen.getByRole("button", { name: t.announcementGotIt }),
    );
    expect(markRead).toHaveBeenCalledWith(2);
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(markRead).toHaveBeenCalledTimes(2);
    // silent 公告始终没有弹出，也没有被标记已读，铃铛还剩 1 条未读。
    expect(
      screen.getByRole("button", { name: "公告，1 条未读" }),
    ).toBeInTheDocument();
  });

  it("shows only a badge on the bell for silent announcements and never pops up", async () => {
    mockApi([
      ann(1, { notifyMode: "silent" }),
      ann(2, { notifyMode: "silent" }),
      ann(3, { notifyMode: "silent", readAt: "2026-02-01T00:00:00Z" }),
    ]);
    render(<AnnouncementCenter t={t} />);

    const bell = await screen.findByRole("button", { name: "公告，2 条未读" });
    expect(within(bell).getByText("2")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("does not pop up popup announcements that are already read on the server", async () => {
    mockApi([ann(1, { readAt: "2026-02-01T00:00:00Z" })]);
    render(<AnnouncementCenter t={t} />);

    await screen.findByRole("button", { name: t.announcementBell });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("closes with Escape and marks the announcement read", async () => {
    const { markRead } = mockApi([ann(1)]);
    render(<AnnouncementCenter t={t} />);

    await screen.findByRole("dialog");
    await userEvent.keyboard("{Escape}");

    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(markRead).toHaveBeenCalledWith(1);
  });

  it("moves focus into the dialog when it opens", async () => {
    mockApi([ann(1)]);
    render(<AnnouncementCenter t={t} />);

    const dialog = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(dialog.contains(document.activeElement)).toBe(true),
    );
  });

  it("stays quiet when fetching fails and recovers on the next trigger", async () => {
    let fail = true;
    const { listAnnouncements } = mockApi(async () => {
      if (fail) throw new Error("network down");
      return [ann(1)];
    });
    render(<AnnouncementCenter t={t} />);

    await waitFor(() => expect(listAnnouncements).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: t.announcementBell }),
    ).toBeInTheDocument();

    fail = false;
    await waitFor(() => expect(changedHandler).not.toBeNull());
    await act(async () => {
      changedHandler?.();
    });

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("公告1")).toBeInTheDocument();
    expect(listAnnouncements).toHaveBeenCalledTimes(2);
  });

  it("refetches when the background poller reports a change and pops up the new announcement", async () => {
    let list: We2aiAnnouncement[] = [];
    mockApi(async () => list);
    render(<AnnouncementCenter t={t} />);

    await screen.findByRole("button", { name: t.announcementBell });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    list = [ann(5)];
    await waitFor(() => expect(changedHandler).not.toBeNull());
    await act(async () => {
      changedHandler?.();
    });

    expect(
      within(await screen.findByRole("dialog")).getByText("公告5"),
    ).toBeInTheDocument();
  });

  it("does not reuse announcements from the previous session when the identity key changes", async () => {
    const lists: Record<string, We2aiAnnouncement[]> = {
      a: [ann(1)],
      b: [],
    };
    let current = "a";
    const { listAnnouncements } = mockApi(async () => lists[current]);
    const { rerender } = render(<AnnouncementCenter key="a" t={t} />);
    await screen.findByRole("dialog");

    current = "b";
    rerender(<AnnouncementCenter key="b" t={t} />);

    await waitFor(() => expect(listAnnouncements).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(screen.queryByText("公告1")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: t.announcementBell }),
    ).toBeInTheDocument();
  });

  it("ignores a slow response that belongs to the previous session", async () => {
    let resolveOld: ((v: We2aiAnnouncement[]) => void) | undefined;
    let call = 0;
    mockApi(() => {
      call += 1;
      return call === 1
        ? new Promise<We2aiAnnouncement[]>((resolve) => {
            resolveOld = resolve;
          })
        : Promise.resolve([]);
    });
    const { rerender } = render(<AnnouncementCenter key="a" t={t} />);
    await waitFor(() => expect(resolveOld).toBeDefined());

    rerender(<AnnouncementCenter key="b" t={t} />);
    await act(async () => {
      resolveOld?.([ann(9)]);
    });

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.queryByText("公告9")).not.toBeInTheDocument();
  });

  it("lists read and unread announcements from the bell, unread ones marked and newest first", async () => {
    const { markRead } = mockApi([
      ann(1, {
        notifyMode: "silent",
        readAt: "2026-02-01T00:00:00Z",
        content: "旧内容",
      }),
      ann(2, {
        notifyMode: "silent",
        content: "新内容 [官网](https://we2ai.com)",
      }),
    ]);
    render(<AnnouncementCenter t={t} />);

    await userEvent.click(
      await screen.findByRole("button", { name: "公告，1 条未读" }),
    );
    const list = await screen.findByRole("dialog");
    const rows = within(list).getAllByRole("button", { name: /公告\d/ });
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining("公告2"),
      expect.stringContaining("公告1"),
    ]);
    expect(rows[0]).toHaveAttribute("data-unread", "true");
    expect(rows[1]).toHaveAttribute("data-unread", "false");

    // 看已读的：不重复标记。
    await userEvent.click(rows[1]);
    const detail = await screen.findByText("旧内容");
    expect(detail).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: t.announcementGotIt }),
    );
    expect(markRead).not.toHaveBeenCalled();

    // 看未读的：关闭时标记已读，铃铛未读数清零。
    await userEvent.click(
      screen.getByRole("button", { name: "公告，1 条未读" }),
    );
    const list2 = await screen.findByRole("dialog");
    await userEvent.click(within(list2).getByRole("button", { name: /公告2/ }));
    expect(
      await screen.findByRole("link", { name: "官网" }),
    ).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: t.announcementGotIt }),
    );
    expect(markRead).toHaveBeenCalledWith(2);
    expect(
      await screen.findByRole("button", { name: t.announcementBell }),
    ).toBeInTheDocument();
  });

  it("shows an empty state in the bell list when there are no announcements", async () => {
    mockApi([]);
    render(<AnnouncementCenter t={t} />);

    await userEvent.click(
      await screen.findByRole("button", { name: t.announcementBell }),
    );
    expect(
      await screen.findByText(t.announcementListEmpty),
    ).toBeInTheDocument();
  });

  it("closes the bell list with the close button, without a keyboard", async () => {
    mockApi([ann(1, { notifyMode: "silent" })]);
    render(<AnnouncementCenter t={t} />);

    await userEvent.click(
      await screen.findByRole("button", { name: t.announcementBellUnread.replace("{count}", "1") }),
    );
    const list = await screen.findByRole("dialog");
    await userEvent.click(
      within(list).getByRole("button", { name: t.announcementClose }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
  });

  it("does not claim there are no announcements when the first fetch failed", async () => {
    mockApi(async () => {
      throw new Error("offline");
    });
    render(<AnnouncementCenter t={t} />);

    await userEvent.click(
      await screen.findByRole("button", { name: t.announcementBell }),
    );
    await screen.findByRole("dialog");
    expect(screen.queryByText(t.announcementListEmpty)).not.toBeInTheDocument();
  });

  it("blocks the context menu on links so it cannot navigate the WebView, but keeps it on plain text", async () => {
    mockApi([ann(1, { content: "[链接](https://example.com) 普通文字" })]);
    render(<AnnouncementCenter t={t} />);

    const link = await screen.findByRole("link", { name: "链接" });
    expect(fireEvent.contextMenu(link)).toBe(false);
    expect(
      fireEvent.contextMenu(screen.getByText(/普通文字/)),
    ).toBe(true);
  });

  it("drops the announcement being viewed when it disappears, and does not reopen it when it comes back", async () => {
    let list: We2aiAnnouncement[] = [
      ann(1, { notifyMode: "silent", readAt: "2026-01-02T00:00:00Z" }),
    ];
    mockApi(async () => list);
    render(<AnnouncementCenter t={t} />);

    await userEvent.click(
      await screen.findByRole("button", { name: t.announcementBell }),
    );
    await userEvent.click(await screen.findByText("公告1"));
    expect(await screen.findByText(t.announcementGotIt)).toBeInTheDocument();

    list = [];
    await act(async () => changedHandler?.());
    await waitFor(() =>
      expect(screen.queryByText(t.announcementGotIt)).not.toBeInTheDocument(),
    );

    list = [ann(1, { notifyMode: "silent", readAt: "2026-01-02T00:00:00Z" })];
    await act(async () => changedHandler?.());
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByText(t.announcementGotIt)).not.toBeInTheDocument();
  });

  it("does not re-open a dismissed announcement or surface an error when marking read fails", async () => {
    const { markRead, listAnnouncements } = mockApi([ann(1)]);
    markRead.mockRejectedValue(new Error("server down"));
    render(<AnnouncementCenter t={t} />);

    await screen.findByRole("dialog");
    await userEvent.click(
      screen.getByRole("button", { name: t.announcementGotIt }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();

    // 服务端仍显示未读（标记失败）：本会话不再弹出，并会在下次拉取时重试标记。
    await waitFor(() => expect(changedHandler).not.toBeNull());
    await act(async () => {
      changedHandler?.();
    });
    await waitFor(() => expect(listAnnouncements).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await waitFor(() => expect(markRead).toHaveBeenCalledTimes(2));
  });

  it("asks the shell to re-check the session on session-type errors, not on network errors", async () => {
    const onSessionMaybeEnded = vi.fn();
    let error: We2aiApiError = { code: "TRANSIENT", message: "x" };
    mockApi(async () => {
      throw error;
    });
    render(
      <AnnouncementCenter t={t} onSessionMaybeEnded={onSessionMaybeEnded} />,
    );
    await waitFor(() =>
      expect(we2aiApi.listAnnouncements).toHaveBeenCalledTimes(1),
    );
    await act(async () => {});
    expect(onSessionMaybeEnded).not.toHaveBeenCalled();

    error = { code: "NETWORK_ERROR", message: "x" };
    await waitFor(() => expect(changedHandler).not.toBeNull());
    await act(async () => {
      changedHandler?.();
    });
    expect(onSessionMaybeEnded).not.toHaveBeenCalled();

    error = { code: "TOKEN_REVOKED", message: "revoked" };
    await act(async () => {
      changedHandler?.();
    });
    await waitFor(() => expect(onSessionMaybeEnded).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("also re-checks the session when marking read fails with a session error", async () => {
    const onSessionMaybeEnded = vi.fn();
    const { markRead } = mockApi([ann(1)]);
    markRead.mockRejectedValue({ code: "NO_ACTIVE_SESSION", message: "x" });
    render(
      <AnnouncementCenter t={t} onSessionMaybeEnded={onSessionMaybeEnded} />,
    );

    await screen.findByRole("dialog");
    await userEvent.click(
      screen.getByRole("button", { name: t.announcementGotIt }),
    );

    await waitFor(() => expect(onSessionMaybeEnded).toHaveBeenCalledTimes(1));
  });

  it("keeps a dismissed announcement read when an older in-flight response returns unread", async () => {
    let call = 0;
    let resolveSecond: ((v: We2aiAnnouncement[]) => void) | undefined;
    mockApi(() => {
      call += 1;
      return call === 1
        ? Promise.resolve([ann(1)])
        : new Promise<We2aiAnnouncement[]>((resolve) => {
            resolveSecond = resolve;
          });
    });
    render(<AnnouncementCenter t={t} />);
    await screen.findByRole("dialog");

    // 第二次拉取在途时关闭弹窗，随后在途响应带着服务端 readAt=null 返回。
    await waitFor(() => expect(changedHandler).not.toBeNull());
    await act(async () => {
      changedHandler?.();
    });
    await waitFor(() => expect(resolveSecond).toBeDefined());
    await userEvent.click(
      screen.getByRole("button", { name: t.announcementGotIt }),
    );
    await act(async () => {
      resolveSecond?.([ann(1)]);
    });

    expect(
      await screen.findByRole("button", { name: t.announcementBell }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("does not close or mark read when the backdrop is clicked", async () => {
    const { markRead } = mockApi([ann(1)]);
    render(<AnnouncementCenter t={t} />);
    await screen.findByRole("dialog");

    const overlay = document.querySelector('[data-state="open"].fixed.inset-0');
    expect(overlay).not.toBeNull();
    await userEvent.click(overlay as Element);

    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(markRead).not.toHaveBeenCalled();
  });

  it("unsubscribes from the change event on unmount", async () => {
    const unlisten = vi.fn();
    vi.spyOn(we2aiApi, "onAnnouncementsChanged").mockResolvedValue(unlisten);
    mockApi([]);
    const { unmount } = render(<AnnouncementCenter t={t} />);
    await screen.findByRole("button", { name: t.announcementBell });
    await act(async () => {});

    unmount();

    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});
