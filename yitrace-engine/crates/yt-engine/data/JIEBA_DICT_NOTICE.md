# jieba_dict.txt 来源与许可

`jieba_dict.txt` 是 **jieba 中文分词**（结巴）项目的全量词典 `dict.txt`，编进引擎二进制供
`ChineseTokenizer` 默认加载（见 `src/tokenizer_cn.rs`）。

- 项目：https://github.com/fxsjy/jieba
- 许可：**MIT License**（可自由使用/再分发，含商业与私有化部署，保留版权声明即可）
- 格式：每行 `词 频次 [词性]`
- 获取：从上游 `jieba/dict.txt` 原样取得（34.9 万词）

> MIT 属于信创认可的开源许可，私有化/气隙部署无再分发障碍（见产品说明合规小节）。
> 自有领域词在 `tokenizer_cn.rs` 的 `DOMAIN_DICT` 里叠加；运行时自有词典走 `with_user_dict`。
