# 签名静态基线

签名静态基线用于在没有实时官方配对时，对小狸单 Token 指纹做**低置信参考比较**。签名验证只说明文件由本机已信任的发布密钥签发；它不证明样本采集诚实、API 物理模型真实，也不能单独把审计总裁决设为 `consistent`。

## 信任流程

1. 从发布者以独立渠道取得 Ed25519 公钥文件：

   ```json
   {
     "keyId": "publisher-key-2026",
     "label": "Example baseline publisher",
     "publicKeyBase64": "<32-byte Ed25519 public key in base64>",
     "createdAt": "2026-08-27T00:00:00Z"
   }
   ```

2. 在工作台“参考资料”页明确确认并导入信任锚。
3. 再选择签名基线包并导入。包内不允许出现 `publicKey` 或 `publicKeyBase64`；小狸只按 `keyId` 查找此前独立导入的本地信任锚。
4. 工作台只有在验签成功、协议/精确模型/探针参数匹配且未过期时，才显示“已验证 · 低置信 scorer”。
5. 撤销信任锚会事务性移除由该 key 验证的 scorer 包。审计完成、写入数据库和发给 UI 之前还会对精确的 `keyId + baselineId + verifiedAt` 重验；若审计进行期间信任锚被撤销或包被替换，本次静态身份评分会被清除并降为“证据不足”。已经完成的历史报告保留为当时证据，新审计不能继续使用被撤销的包。

## 包格式 v1

```json
{
  "schemaVersion": 1,
  "algorithm": "ed25519",
  "keyId": "publisher-key-2026",
  "payload": {
    "id": "community-gpt56-sol-2026-08",
    "label": "GPT-5.6 Sol August reference",
    "source": "community",
    "version": "2026.08",
    "model": "gpt-5.6-sol",
    "protocol": "openAiResponses",
    "sampleCount": 40,
    "createdAt": "2026-08-27T00:00:00Z",
    "expiresAt": "2026-11-27T00:00:00Z",
    "parameters": {
      "generatorVersion": "xiaoli-fingerprint-v1",
      "normalizationVersion": "xiaoli-one-word-v1",
      "temperatureMilli": 1000,
      "maxOutputTokens": 16,
      "sameModelMaxMeanJsdMicros": 120000,
      "differentModelMinMeanJsdMicros": 300000
    },
    "fingerprintCells": [
      {
        "family": "number",
        "language": "english",
        "counts": { "7": 10 }
      },
      {
        "family": "letter",
        "language": "english",
        "counts": { "a": 10 }
      },
      {
        "family": "color",
        "language": "english",
        "counts": { "blue": 10 }
      },
      {
        "family": "animal",
        "language": "english",
        "counts": { "cat": 10 }
      }
    ],
    "limitations": ["example only"]
  },
  "signatureBase64": "<64-byte Ed25519 signature in base64>"
}
```

实际包必须包含 4–40 个不重复 cell，每个 cell 至少 10 个、最多 1000 个有效样本；`sampleCount` 必须与全部计数精确相等。输出标签必须已经通过小狸的一词规范化。阈值、版本、模型、协议、时效和计数分布全部位于签名覆盖的 payload 内。

可用于 scorer 的包必须提供 `expiresAt`。当前常量允许 `createdAt` 最多领先本机时钟 24 小时，且 `expiresAt - createdAt` 最多 180 天；超出范围的包即使发布者签名有效，也只能作为已验签元数据，不能参与评分。同一签名者和 `payload.id` 下，完整相同的签名内容可幂等重复导入；不同内容只有在签名覆盖的 `createdAt` 严格更新时才能替换，等时替换和旧包回滚都会被拒绝。

## 签名字节

v1 签名字节为：

```text
ASCII("XiaoLi relay baseline v1\\0") || UTF8(serde_json(payload))
```

payload 先反序列化为固定的、拒绝未知字段的 v1 类型，再按字段定义顺序序列化；所有计数 map 使用字典序。发布工具必须用与小狸相同的类型和测试向量生成签名，不能直接对任意原始 JSON 文本签名。

## 评分边界

- 静态包只参与身份指纹轴，不替代实时官方配对的质量、usage 或中/高置信身份统计。
- “与静态参考一致”仍只能得到低置信证据，不能单独产生绿色总裁决。
- “与静态参考显著不同”可以成为低置信异常证据，但仍不能命名实际模型。
- 每次读取重新验签并检查过期时间；数据库中的中转观测不能更新可信包表。
- 公开检测方法仍可能被中转识别并选择性诚实；签名基线不消除黑盒审计的可规避性。
