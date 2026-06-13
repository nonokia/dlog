# Agent-First 意思決定ログ / バージョン管理ツール 設計検討メモ

作成日: 2026-06-12（2026-06-13 更新・第5版）
ステータス: 初期検討（データモデル・Git統合・クエリAPI・ASTアンカーまで方針確定。OpenSpec 移行可能段階）

---

## 1. 背景・モチベーション

- Zed Industries が開発中の **DeltaDB**（操作ベースのバージョン管理、CRDTで全編集をリアルタイム記録、Git互換、キャラクターレベルパーマリンク）に近い課題意識を独立に持っていた
- Cursor が RFC として公開した **Agent Trace**（AI生成コードの帰属を記録するベンダーニュートラル仕様、Cognition / Cloudflare / Vercel / Google Jules 等が賛同）が土台として利用できそう
- 収益化は考えず、**興味本位の OSS** として検証する

### DeltaDB との違い（本構想のポジション）

| | DeltaDB | 本構想 |
|---|---|---|
| 主眼 | 人間とAgentの協調 | **完全Agent-first** |
| 同期 | CRDTによるリアルタイム分散同期 | ローカル完結（分散同期なし） |
| 位置づけ | Gitの置き換え・拡張（重量級） | **Gitの横に置く軽量な判断レイヤー** |
| インターフェース | エディター（Zed）密結合 | Agent向けAPIのみ。人間はAgent経由で参照 |

CRDT は DeltaDB の実装選択であって踏襲しない。ローカルの複数プロセス程度なら append-only ログ + 単一書き込み点（SQLite等）で十分。

---

## 2. コア思想

### 2.1 Agent-first 設計

- 書き込み・読み込みは基本的に **Agent のみ**が行う
- 人間は Agent の挙動や成果物に疑問を持ったとき、**Agent 経由で参照**する程度
- 人間可読性・人間向けUI（checkout / merge / branch 等の概念）は優先しない
- スキーマは Agent が生成・消費しやすい形を優先
- コンテキストウィンドウに収まる形で「必要な履歴だけ」を取り出せるクエリインターフェースが肝

### 2.2 記録の単位は「判断」

編集操作でもコミットでもなく、**1つの判断（decision）** を記録単位とする。

Git が保存するもの vs Agent が本当に欲しいもの：

| 情報 | Gitにあるか |
|---|---|
| 何が変わったか（diff） | ある |
| **なぜ**変えたか | コミットメッセージに少しだけ |
| **何を試して捨てたか** | ない |
| **どんな前提を置いたか** | ない |
| **元の指示は何だったか** | ない |

→ 「バージョン管理」というより **意思決定ログ + 前提条件DB**。
→ diff の保存は Git に任せ、本ツールは「コード位置 ⇔ 判断・前提・指示」のマッピングに特化する割り切りもアリ。

---

## 3. Agent のユースケースと必要情報

| 場面 | 必要な情報 |
|---|---|
| 変更前の「なぜこうなってるか」確認 | その行を書いた時の制約条件・却下された代替案 |
| 過去の失敗の回避 | 試行→失敗→修正の履歴（捨てた実装と捨てた理由） |
| タスク開始時のコンテキスト復元 | ファイル/モジュール単位の意思決定サマリ（圧縮された結論） |
| 他Agentとの暗黙の連携 | 不変条件・前提（invariants）の宣言 |
| 人間への説明 | 会話ID→元の指示まで遡れるトレーサビリティチェーン |

---

## 4. マルチエージェント対応

オーケストレーター + サブエージェント構成が主流になっているため、**複数Agentのローカル並行作業**を最初から設計に含める。

追加で必要なデータモデル要素：

1. **Agentのアイデンティティ**
   - モデル名だけでなく役割（reviewer / implementer 等）
   - Agent Trace の `contributor` は human/ai 区別程度なので拡張が必要
2. **タスクの階層構造**
   - 親タスク→子タスクの系譜（`parent_task_id` 等）
   - オーケストレーターの委譲で文脈が切れないように
3. **Agent間の因果関係**
   - 「BはAのレビュー指摘を受けた修正」のような判断の連鎖
   - 判断同士は フラットなログではなく **DAG（グラフ構造）** になる

解決したい既存課題の例：Claude Code のサブエージェントは親に要約しか返さないため、子Agentの現場判断が消失する → 本ツールで永続化できる。

---

## 5. 想定データモデル（ラフスケッチ）

> **Note**: 本章は初期スケッチ。7章のデータモデル詳細設計で更新されている（エンティティ3分割・binding 等）。

```
Decision（判断）レコード
├─ id
├─ parent_task_id          # タスク階層
├─ caused_by_decision_id[] # 判断間の因果（DAG）
├─ agent:
│   ├─ role（reviewer / implementer など）
│   ├─ model
│   └─ session_id
├─ conversation_id          # Agent Trace 互換のコンテキストリンク
├─ files[]: path + line ranges
├─ rationale                # なぜそうしたか
├─ rejected_alternatives[]  # 試して捨てた案と理由
├─ assumptions / invariants # 置いた前提・不変条件
└─ timestamp
```

ストレージ: append-only ログ。実装候補は SQLite（複数プロセスの書き込み調停が容易）。

---

## 6. 技術方針

- **Agent Trace 仕様を土台に拡張フィールドで実験**（仕様はストレージ非依存・metadata はベンダー拡張可なので相性が良い）
- CRDT は採用しない（リアルタイム分散同期が不要なため）
- Git とは併存。diff 管理は Git、判断・文脈管理は本ツール
- Agent Trace は RFC 段階（v0.1.0）であり仕様変更の可能性がある点は留意

### 6.1 インターフェース形態（確定）

**hooks / MCP ではなく、独立した CLI アプリケーション**としてビルドし、Agent にコマンドとして使わせる。

- 最近のコーディング Agent は bash を自在に使えるため、CLI が最も自然なインターフェース
- 特定の Agent 製品（Claude Code 等）に依存せず、Cursor / Devin / 自作 Agent でも同様に使える
- 入出力は完全に JSON 前提。人間向けの整形出力（カラー、テーブル等）は不要
- デーモン不要。各 CLI 呼び出しが SQLite を直接叩く。並行書き込みは SQLite のロックで調停
- Agent への「使い方の教え込み」が必要 → **ツール本体と一緒に CLAUDE.md / AGENTS.md 用インストラクションテンプレートを配布**する

コマンド体系のラフイメージ：

```bash
# 判断の記録
dlog record --task <task_id> --files src/auth.ts:10-45 \
  --rationale "..." --rejected "..." --assumes "..."

# 参照系
dlog why src/auth.ts:23          # この行の判断経緯
dlog context src/auth/           # モジュールの意思決定サマリ
dlog invariants                  # 宣言済みの前提一覧
dlog trace <decision_id>         # 因果チェーンを遡る
```

### 6.2 コード位置の追従：AST ノードアンカー方式（方針確定）

検討課題だった「行番号はリファクタで壊れる」問題への答えとして、**判断のアンカーを行番号ではなく AST ノードに紐付ける**。

- `src/auth.ts:10-45` ではなく「`authenticate` 関数のノード」に判断を紐付ける
- 関数の移動・前後への行挿入に耐える。DeltaDB のキャラクターレベルパーマリンクの軽量近似を CRDT なしで実現
- フォーマット・インデント変更を「変更なし」として扱え、Agent が読む diff のトークン数も節約できる
- 実装は **tree-sitter を直接利用**する（difftastic は人間の目視用設計で、出力の機械パースは作者非推奨のため、表示ツールではなくその基盤技術を使う）

### 6.3 実装言語：Rust（確定）

- tree-sitter（Rust ライブラリとして組み込み可能）との親和性が決め手
- CLI + SQLite + tree-sitter という構成は Rust エコシステムが最も充実している領域
  - CLI: clap / シリアライズ: serde / SQLite: rusqlite
- 作者の Rust 学習も兼ねる（知見ゼロからのスタート。学習題材としても好適）

---

## 7. データモデル詳細設計（方針確定）

### 7.1 エンティティは3分割

Decision 一枚岩ではなく、寿命と参照パターンが異なる3エンティティに分ける：

- **Decision**: 判断の記録。append-only の本ログ
- **Task**: タスク階層（`parent_task_id`）と人間の元指示を保持
- **Invariant**: 不変条件。宣言した Decision より寿命が長く、`dlog invariants` で単独クエリされる。出自として宣言元 Decision へのリンクを持つ

### 7.2 判断の上書きは supersedes 方式

append-only のため UPDATE はしない。判断が覆った場合は新 Decision が `supersedes: <old_decision_id>` を持つ。最新のみ返すか履歴ごと返すかはクエリ側オプション。

### 7.3 必須フィールド最小主義

記録のフリクションが高いと Agent が書かなくなる／適当な値で埋め始めるため、必須は `rationale` + アンカー + agent 識別程度に絞る。`rejected` / `assumptions` は任意。

### 7.4 Decision スキーマドラフト

```jsonc
{
  "id": "dec_01J...",            // ULID（時系列ソート可能）
  "task_id": "tsk_01J...",
  "caused_by": ["dec_..."],      // 因果DAG
  "supersedes": "dec_... | null",
  "agent": {
    "role": "implementer",
    "model": "claude-...",
    "session_id": "..."
  },
  "conversation_id": "...",      // Agent Trace互換
  "anchors": [ /* ASTノードアンカー + 行範囲スナップショット併記 */ ],
  "rationale": "...",            // 必須
  "rejected": [{ "approach": "...", "reason": "..." }],
  "declares_invariants": ["inv_..."],
  "binding": { /* 8.2 参照 */ },
  "timestamp": "..."
}
```

---

## 8. Git 統合モデル（方針確定）

### 8.1 二層構造のポジショニング

- **人間**: git レイヤーで「何が変わったか」を読む（従来通り）
- **Agent**: 本ツールのレイヤーまで潜り、思考・実装履歴・意思決定の証跡をたどる

git 全体を透過プロキシするラッパーにはしない（`git log` / `rebase` 等の再実装は泥沼）。統合点はコミットの瞬間のみ。

### 8.2 ステージング + シール方式

**判断はコミットより先に生まれる**（捨て案はコミットに到達しない）ため、コミット時のみの記録は不可。かといって本ログに「sha 未確定」レコードを許すと null の曖昧さ問題が出る。解は git の index と同型のステージング：

1. タスク中の判断は即座に **staging テーブル**（同一 SQLite 内、書き換え自由な作業領域）へ書く → クラッシュしても消えない
2. シール時に binding を刻んで**不可侵の本ログへ移す**
3. 本ログのレコードは必ず明示的な binding を持つ：

```
binding:
  { type: "commit", sha: "..." }   # コード変更に至った判断
  { type: "none" }                  # コミットに繋がらない判断（調査・レビュー等）
# 「保留中」という状態は本ログに存在しない（staging にいること = 保留）
```

### 8.3 シールのトリガーは2系統

- **コード系**: `dlog commit`（git commit 実行 + staging を新 sha でシール）または `dlog bind <sha>`
- **非コード系**: タスク完了時（`dlog task done` 等）に `binding: none` でシール。サブエージェントは自タスク終了時に必ずシールするルールにすることで、親に要約しか返らなくても判断の現物が本ログに残る

素の `git commit` をされて staging が滞留した場合は、`dlog status` で検出可能にし、Agent 自身に処理させる（タスク開始時に status を確認する運用をインストラクションテンプレート側に記載。9.4 参照）。

### 8.4 実装フェーズ分割

スキーマの後付けは高くつくがコマンドの後付けは安い、という非対称性に基づき：

- **v0.1（PoC）**: staging / 本ログ / binding のスキーマ一式 + 手動の `dlog bind <sha>`（実装数行）
- **v0.2 以降**: `dlog commit` ラッパー本体、post-commit フックでの自動バインド（リポジトリ側の仕組みなので Agent 製品非依存。オプション機能扱い）

---

## 9. クエリ API 設計（方針確定）

### 9.1 設計原則

**原則1：二段階取得（compact → drill-down）**

デフォルトは圧縮形（id + rationale 要約 + binding + 日時）のリストを返し、Agent が必要と判断したものだけ `dlog show <id>` で全文取得。コンテキストウィンドウの節約が目的。git の `log --oneline` → `show` と同型。

**原則2：レスポンスは提案ではなく状態で自己記述する**

Agent が導出できない事実（`resolution`、`truncated` 等）のみをデータとして返す。

> **却下した代替案: hints フィールド**
> レスポンスに「次に実行すべきコマンド例」を埋め込む案を検討したが却下。理由: (1) 結果に id が含まれる時点で次の操作は Agent が導出可能であり、コマンド体系はインストラクションテンプレート側で教える設計のため純粋な重複（原則1と自己矛盾）。(2) ツールが毎回「次の一手」を囁くのは Agent の自律的なプラン形成へのアンカリングになる。本ツールは Agent の判断を記録する側であって誘導する側ではない。

**原則3：デフォルトスコープは「生きている判断」**

- superseded された判断はデフォルト除外（`--include-superseded` で履歴込み）
- staging は**デフォルトで含める**（直前の判断こそ参照価値が高い）。`staged: true` フラグで区別
- 本ログと staging の UNION はツール内部で吸収し、Agent には単一リストとして見せる

### 9.2 コマンド体系

```bash
dlog why <file:line | シンボル名>   # この位置・この関数の判断経緯
dlog show <id>...                   # 判断の全文（rejected / assumptions 含む）
dlog context <path>                 # ファイル / ディレクトリ単位のサマリ
dlog trace <id> [--depth N]         # 因果 DAG を遡る / 下る
dlog invariants [--scope <path>]    # 生きている不変条件
dlog search --text "..."            # 全文検索(SQLite FTS5。ほぼ無コストで載るため v0.1 に含める)
dlog status                         # ストア状態(staging 未シール件数・最古の滞留・スキーマバージョン等)
```

- `why` の入力は file:line とシンボル名の両対応(コードを読んだ直後の Agent は行番号を、会話文脈からはシンボル名を使うため)
- アンカー解決に失敗した場合はエラーにせず、ファイルレベルの判断に格下げして返す(空振りで Agent を止めない)

### 9.3 レスポンスエンベロープ

```jsonc
{
  "query": { "type": "why", "anchor": "src/auth.ts:23" },
  "resolved": { "node": "fn authenticate", "resolution": "exact" },
  "results": [
    { "id": "dec_01J...", "rationale_summary": "リトライ追加。API不安定対策",
      "binding": { "type": "commit", "sha": "a3f..." },
      "staged": false, "superseded": false, "ts": "..." }
  ],
  "truncated": false
}
```

- `resolution`: クエリ解決の品質（「この結果は質問にどの程度正確に答えているか」）。enum 値の確定は AST アンカー同一性設計とセットで行う（`exact` / `file_fallback` 等を想定）
- クエリレスポンスに含める警告は「この結果の品質に関わるもの」のみ（例: staging が読めず結果が不完全）。ストア全体の状態は `dlog status` に分離

### 9.4 status の参照タイミングは運用側に委ねる

`dlog status` をいつ確認するかはツールが押し付けず、インストラクションテンプレートに「タスク開始時に status を確認」と記載する運用とする。

---

## 10. AST ノードアンカーの同一性設計（方針確定）

### 10.1 同一性は「保存する事実」ではなく「クエリ時に解決する判定」

記録時点で将来の変更は予知できないため、保存時に同一性を確定しようとすると詰む。発想を逆転し、**アンカーには記録時点の観測値だけを残し、同一性判定はクエリ時に行って確度を `resolution` として Agent に開示する**。曖昧さごと Agent に渡してよい（9章 原則2「状態で自己記述」の延長）。

### 10.2 アンカーに保存する観測値

```jsonc
{
  "file": "src/auth.rs",
  "symbol_path": "AuthService::authenticate",  // 名前ベースの座標
  "node_kind": "function",
  "structural_hash": "h_...",   // 識別子名・コメント・空白を除いた正規化トークン列のハッシュ
  "line_span": [10, 45],        // 人間向けスナップショット（解決には使わない）
  "recorded_at_sha": "..."      // binding 経由で記録時点のコードに到達可能
}
```

### 10.3 クエリ時の照合は2軸マトリクス

| symbol_path | structural_hash | resolution | 意味 |
|---|---|---|---|
| 一致 | 一致 | `exact` | そのまま |
| 一致 | 不一致 | `drifted` | 同名だが中身が進化（判断が陳腐化している可能性を示唆） |
| 不一致 | 一致 | `relocated` | リネーム・移動された同一実体 |
| 不一致 | 不一致 | `file_fallback` | ノード特定不能、ファイルレベルに格下げ |

- `structural_hash` 照合は**ファイル横断（グローバル）**。関数が別ファイルへ移動されても追える。SQLite のインデックス1本で済むためコストは無視できる
- 「リネーム + 中身も大幅変更」は `file_fallback` に落ちるが割り切る。鮮度の落ちた判断であり、ファイルレベルでは依然浮上するため完全消失はしない（git のリネーム検出も類似度ヒューリスティクスという「諦めを含む設計」）
- これで9章の `resolution` enum が確定：`exact` / `drifted` / `relocated` / `file_fallback`

### 10.4 アンカー可能なノードの粒度

任意の式ではなく**名前を持つ定義ノード**（関数・メソッド・struct/class・モジュール）に限定。tree-sitter の tags.scm 形式（GitHub コードナビ互換）の定義抽出クエリに乗る。`why file:line` は「その行を包む最内の定義ノード」に解決する。

### 10.5 責務分解：記録は言語非依存、アンカー解決のみ言語依存

- **言語非依存（ツール本体）**: Decision/Task/Invariant の記録・staging・シール・binding、show/trace/invariants/search/status。**ファイルレベルのアンカーも言語非依存**（パス記録のみ。コード以外の YAML/Markdown 等への判断も記録可能）
- **言語依存（アンカー解決レイヤーのみ）**: `symbol_path` と `structural_hash` の抽出。tree-sitter は言語ごとに grammar が別でバイナリ組み込みのため、「対応言語」概念はここにだけ発生
- **未対応言語は自然 degrade**: grammar がない言語のファイルは常に `file_fallback` 相当。記録も参照も普通に動く。ノード追跡は言語ごとに後から増やせる拡張（grammar 追加はバイナリサイズとビルド依存が増えるだけの「安い後付け」）

### 10.6 最初にノードアンカーを有効化する言語：Rust（確定）

dlog 自身が Rust 製のため、dlog 開発に dlog を使う**ドッグフーディング**が初日から回る。TypeScript（Agent 案件の最大票田）は他人に配る v0.2 以降で追加。

---

## 11. 次の検討課題

- [x] ~~Decision レコードの具体的スキーマ定義~~ → エンティティ3分割・supersedes・必須最小主義・binding enum で方針確定（7章）。JSON Schema 化は実装タスクとして残る
- [x] ~~Agent 向けクエリAPIの設計~~ → 二段階取得・状態による自己記述・デフォルトスコープ・コマンド7種で方針確定（9章）
- [x] ~~書き込みのトリガー設計~~ → 独立 CLI として確定（6.1）。記録タイミングはステージング + シール方式で確定（8.2, 8.3）
- [x] ~~コード位置の追従方法~~ → AST ノードアンカー方式・tree-sitter ベースで方針確定（6.2）
- [x] ~~AST ノードアンカーの具体設計~~ → クエリ時解決・観測値スキーマ・2軸照合マトリクス・言語依存分離で方針確定（10章）。`resolution` enum も確定（`exact`/`drifted`/`relocated`/`file_fallback`）
- [ ] コンテキスト圧縮戦略（履歴をどう要約してコンテキストウィンドウに収めるか）※v0.1 スコープ外。実物が動いてから
- [ ] CLI コマンド体系の詳細設計と Agent 向けインストラクションテンプレートの作成 ※OpenSpec の capability 単位に対応
- [ ] PoC のスコープと最小構成（Rust プロジェクトの雛形。`dlog record` + `dlog why` の2コマンドを最初のマイルストーンに）

---

## 12. OpenSpec 移行メモ

設計思想（Why）が固まり、仕様（What）に落とせる段階に到達。OpenSpec で capability 単位に分解する際の想定：

- **capability: decision-log** — 記録（record）・staging・supersedes
- **capability: git-binding** — シール・binding enum・`dlog bind`（`dlog commit` は v0.2 の change proposal）
- **capability: query** — why/show/context/trace/invariants/search/status
- **capability: ast-anchor** — 観測値抽出・クエリ時解決・resolution 判定（Rust から）

v0.1 で切るスコープ: コンテキスト圧縮、`dlog commit` ラッパー、post-commit 自動バインド、Rust 以外の grammar。

---

## 参考リンク

- DeltaDB 発表（Zed blog）: https://zed.dev/blog/sequoia-backs-zed
- Agent Trace 仕様: https://agent-trace.dev/
- Agent Trace GitHub: https://github.com/cursor/agent-trace
- Cognition の Agent Trace 解説: https://cognition.ai/blog/agent-trace
