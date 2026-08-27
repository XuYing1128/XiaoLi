import { readFile } from "node:fs/promises";
import { describe, expect, it } from "vitest";

describe("XiaoLi in-app status guide contract", () => {
  it("keeps the three route outcomes and connection-origin evidence in the monitor guide", async () => {
    const source = await readFile("src/main.ts", "utf8");

    expect(source).toContain("服务器已重路由");
    expect(source).toContain("已重路由，目标未知");
    expect(source).toContain("未见服务器重路由");
    expect(source).toContain("显示官方登录、官方 API、自定义、本地或未知端点");
    expect(source).toContain("不会根据请求值、速度或文本特征补猜目标");
  });
});
