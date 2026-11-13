"""每轮用户消息时附上表达纪律要点（UserPromptSubmit 钩子）。

输出一段 JSON：{"additionalContext": "<纪律文本>"}，ZCode 会把它注入对话上下文。
"""

import json
import sys

DISCIPLINE = """【表达纪律·每轮生效】
1. 说白话：完整直白的陈述句，不用比喻、类比、拟人。
2. 不造新词；新概念第一次出现，必须用一句现成的话解释清楚。
3. 引用编号（M1、D6、P22 这类）必须同时给全称和一句话说明，不许只甩编号。
4. 一次一个决策：逐项讨论，不把多个未决问题打包抛给用户。
5. 发出前自检：这句话里有没有只有我自己懂的说法，有就重写成白话。"""


def main() -> int:
    payload = json.dumps({"additionalContext": DISCIPLINE}, ensure_ascii=False)
    # 直接写字节，避开 Windows 控制台默认 GBK 编码导致中文变乱码
    sys.stdout.buffer.write(payload.encode("utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
