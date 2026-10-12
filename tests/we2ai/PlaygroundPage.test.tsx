import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  we2aiApi,
  type We2aiKeyList,
  type We2aiPlaygroundChatOutcome,
  type We2aiPlaygroundChatRequest,
  type We2aiPlaygroundChunkEvent,
  type We2aiPlaygroundModels,
} from "@/we2ai/api";
import { PlaygroundPage } from "@/we2ai/PlaygroundPage";
import { getWe2aiStrings } from "@/we2ai/strings";

// Radix Select 打开时会滚动到选中项，jsdom 没有实现 scrollIntoView。
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

const t = getWe2aiStrings("zh");

const KEY_LIST: We2aiKeyList = {
  keys: [
    { id: 5, name: "工作", groupName: null, status: "active", maskedKey: "sk-we2…5555" },
    { id: 6, name: "备用", groupName: null, status: "active", maskedKey: "sk-we2…6666" },
    { id: 7, name: "旧Key", groupName: null, status: "quota_exhausted", maskedKey: "sk-we2…7777" },
  ],
  selectedKeyId: 5,
  rememberedKeyId: null,
  studioSelectedKeyId: 5,
};

const MODELS: We2aiPlaygroundModels = {
  models: [{ id: "gpt-marketing", provider: null }],
  blockedReason: null,
};

const OUTCOME: We2aiPlaygroundChatOutcome = {
  finishReason: "stop",
  usage: { promptTokens: 10, completionTokens: 6, totalTokens: 16 },
};

const onSessionMaybeEnded = vi.fn();
const onTerminal = vi.fn();

/** 捕获页面订阅的 chunk 回调，供用例模拟 Rust 增量事件。 */
let chunkHandler: ((event: We2aiPlaygroundChunkEvent) => void) | null = null;

async function renderPage() {
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(
      <PlaygroundPage
        t={t}
        language="zh"
        onSessionMaybeEnded={onSessionMaybeEnded}
        onTerminal={onTerminal}
      />,
    );
  });
  return view;
}

/** 打开 Radix Select 并点击一个选项（按可见文本定位）。 */
async function selectOption(testId: string, label: string) {
  await userEvent.click(screen.getByTestId(testId));
  const listbox = await screen.findByRole("listbox");
  const option = await within(listbox).findByText(label);
  await userEvent.click(option);
}

describe("PlaygroundPage（功能 27 P1）", () => {
  let chatRequest: We2aiPlaygroundChatRequest | null = null;

  beforeEach(() => {
    vi.restoreAllMocks();
    chunkHandler = null;
    chatRequest = null;
    onSessionMaybeEnded.mockClear();
    onTerminal.mockClear();

    vi.spyOn(we2aiApi, "listKeys").mockResolvedValue(KEY_LIST);
    vi.spyOn(we2aiApi, "playgroundModels").mockResolvedValue(MODELS);
    vi.spyOn(we2aiApi, "playgroundChooseKey").mockResolvedValue({
      studioSelectedKeyId: 6,
    });
    vi.spyOn(we2aiApi, "playgroundCancel").mockResolvedValue(undefined);
    vi.spyOn(we2aiApi, "onKeysChanged").mockResolvedValue(() => {});
    vi.spyOn(we2aiApi, "onPlaygroundChunk").mockImplementation(
      async (fn) => {
        chunkHandler = fn;
        return () => {
          chunkHandler = null;
        };
      },
    );
    vi.spyOn(we2aiApi, "playgroundChat").mockImplementation(
      async (request) => {
        chatRequest = request;
        // 发送后立即推送两个增量，模拟 Rust 流式事件。
        chunkHandler?.({ requestId: request.requestId, delta: "第一段" });
        chunkHandler?.({ requestId: request.requestId, delta: "第二段" });
        return OUTCOME;
      },
    );
  });

  it("默认选中工坊记忆的 Key 并加载模型", async () => {
    await renderPage();
    expect(await screen.findByTestId("we2ai-studio-key-select")).toHaveTextContent(
      "工作",
    );
    await waitFor(() => {
      expect(we2aiApi.playgroundModels).toHaveBeenCalledWith(5);
    });
    expect(await screen.findByTestId("we2ai-studio-model-select")).toHaveTextContent(
      "gpt-marketing",
    );
  });

  it("额度耗尽的 Key 在选择器中禁用", async () => {
    await renderPage();
    await userEvent.click(await screen.findByTestId("we2ai-studio-key-select"));
    const listbox = await screen.findByRole("listbox");
    const disabled = within(listbox).getByText(/旧Key/);
    expect(disabled.closest('[role="option"]')).toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("点击场景预设发送 system，流式增量渲染并显示 usage", async () => {
    await renderPage();
    await waitFor(() => {
      expect(we2aiApi.playgroundModels).toHaveBeenCalled();
    });

    await userEvent.click(
      await screen.findByTestId("we2ai-studio-preset-product-copy"),
    );
    const input = await screen.findByTestId("we2ai-studio-input");
    await userEvent.type(input, "智能水杯");
    await userEvent.click(screen.getByTestId("we2ai-studio-send"));

    await waitFor(() => {
      expect(chatRequest).not.toBeNull();
    });
    const request = chatRequest!;
    expect(request.keyId).toBe(5);
    expect(request.model).toBe("gpt-marketing");
    expect(request.system).toContain("产品文案");
    expect(request.messages).toEqual([
      { role: "user", content: "智能水杯" },
    ]);

    const bubbles = await screen.findAllByTestId("we2ai-studio-assistant-message");
    expect(bubbles[0]).toHaveTextContent("第一段第二段");
    // usage 展示（总 tokens）。
    expect(bubbles[0]).toHaveTextContent("16 tokens");
    // 终态回调：外壳刷新余额。
    expect(onTerminal).toHaveBeenCalled();
  });

  it("多轮历史随请求重发；错误轮与停止轮不进历史", async () => {
    await renderPage();
    await waitFor(() => {
      expect(we2aiApi.playgroundModels).toHaveBeenCalled();
    });

    // 第一轮：失败（Key 失效）。
    (we2aiApi.playgroundChat as ReturnType<typeof vi.fn>).mockRejectedValueOnce({
      code: "PLAYGROUND_KEY_INVALID",
      message: "raw",
    });
    const input = screen.getByTestId("we2ai-studio-input");
    await userEvent.type(input, "第一条");
    await userEvent.click(screen.getByTestId("we2ai-studio-send"));
    await waitFor(() => {
      expect(
        screen.getByTestId("we2ai-studio-message-error"),
      ).toHaveTextContent(t.studioErrorKeyInvalid);
    });

    // 第二轮：正常发送——历史只含第二条（错误轮不进历史）。
    await userEvent.type(screen.getByTestId("we2ai-studio-input"), "第二条");
    await userEvent.click(screen.getByTestId("we2ai-studio-send"));
    await waitFor(() => {
      const request = chatRequest!;
      expect(request.messages).toEqual([{ role: "user", content: "第二条" }]);
    });
  });

  it("停止后保留部分内容并标记；停止轮不进下一轮历史", async () => {
    await renderPage();
    await waitFor(() => {
      expect(we2aiApi.playgroundModels).toHaveBeenCalled();
    });

    (we2aiApi.playgroundChat as ReturnType<typeof vi.fn>).mockImplementationOnce(
      async (request: We2aiPlaygroundChatRequest) => {
        chatRequest = request;
        chunkHandler?.({ requestId: request.requestId, delta: "半截" });
        // 模拟用户点了停止：invoke 以 CANCELLED 终态 reject。
        throw { code: "PLAYGROUND_CANCELLED", message: "raw" };
      },
    );
    const input = screen.getByTestId("we2ai-studio-input");
    await userEvent.type(input, "写一半");
    await userEvent.click(screen.getByTestId("we2ai-studio-send"));
    await waitFor(() => {
      expect(screen.getByText(t.studioStoppedPartial)).toBeInTheDocument();
    });
    expect(
      screen.getByTestId("we2ai-studio-assistant-message"),
    ).toHaveTextContent("半截");

    // 下一轮历史不含被停止的轮（user + 半截 assistant 整轮丢弃）。
    await userEvent.type(screen.getByTestId("we2ai-studio-input"), "再来");
    await userEvent.click(screen.getByTestId("we2ai-studio-send"));
    await waitFor(() => {
      const request = chatRequest!;
      expect(request.messages).toEqual([{ role: "user", content: "再来" }]);
    });
  });

  it("切换 Key 开新会话并调用 playgroundChooseKey 记忆", async () => {
    await renderPage();
    await waitFor(() => {
      expect(we2aiApi.playgroundModels).toHaveBeenCalled();
    });

    // 先发一条消息，让会话里有内容。
    const input = screen.getByTestId("we2ai-studio-input");
    await userEvent.type(input, "会被清掉的");
    await userEvent.click(screen.getByTestId("we2ai-studio-send"));
    await waitFor(() => {
      expect(chatRequest).not.toBeNull();
    });

    // 切到另一个 active Key：会话清空，且工坊记忆保存被调用。
    await selectOption("we2ai-studio-key-select", "备用");
    expect(we2aiApi.playgroundChooseKey).toHaveBeenCalledWith(6);
    expect(
      screen.queryByTestId("we2ai-studio-assistant-message"),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("we2ai-studio-key-select")).toHaveTextContent(
      "备用",
    );
  });

  it("会话级错误回调外壳复查会话状态", async () => {
    await renderPage();
    await waitFor(() => {
      expect(we2aiApi.playgroundModels).toHaveBeenCalled();
    });

    (we2aiApi.playgroundChat as ReturnType<typeof vi.fn>).mockRejectedValueOnce({
      code: "SESSION_CHANGED",
      message: "raw",
    });
    await userEvent.type(
      screen.getByTestId("we2ai-studio-input"),
      "触发会话错误",
    );
    await userEvent.click(screen.getByTestId("we2ai-studio-send"));
    await waitFor(() => {
      expect(onSessionMaybeEnded).toHaveBeenCalled();
    });
  });
});
