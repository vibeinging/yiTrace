#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TextDomain {
    Input,
    Output,
    Logs,
    Tool,
    Model,
    Agent,
}

impl TextDomain {
    fn parse(value: &str) -> Option<Self> {
        match value
            .to_ascii_lowercase()
            .replace(['-', '_', '.'], "")
            .as_str()
        {
            "input" | "inputtext" => Some(Self::Input),
            "output" | "outputtext" => Some(Self::Output),
            "log" | "logs" | "logtext" => Some(Self::Logs),
            "tool" | "toolname" => Some(Self::Tool),
            "model" => Some(Self::Model),
            "agent" | "agentname" => Some(Self::Agent),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input_text",
            Self::Output => "output_text",
            Self::Logs => "logs",
            Self::Tool => "tool_name",
            Self::Model => "model",
            Self::Agent => "agent_name",
        }
    }
}

#[derive(Default)]
struct TextDomainIndexes {
    input: Bm25TextIndex,
    output: Bm25TextIndex,
    logs: Bm25TextIndex,
    tool: Bm25TextIndex,
    model: Bm25TextIndex,
    agent: Bm25TextIndex,
}

impl TextDomainIndexes {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn index_record(&mut self, r: &WalRecord) {
        if let Some(text) = r.fields.input_text.as_deref() {
            self.input.index_text(r.trace_id, r.span_id, text);
        }
        if let Some(text) = r.fields.output_text.as_deref() {
            self.output.index_text(r.trace_id, r.span_id, text);
        }
        if !r.fields.logs.is_empty() {
            self.logs
                .index_text(r.trace_id, r.span_id, &r.fields.logs.join(" "));
        }
        if let Some(text) = r.fields.tool_name.as_deref() {
            self.tool.index_text(r.trace_id, r.span_id, text);
        }
        if let Some(text) = r.fields.model.as_deref() {
            self.model.index_text(r.trace_id, r.span_id, text);
        }
        if let Some(text) = r.fields.agent_name.as_deref() {
            self.agent.index_text(r.trace_id, r.span_id, text);
        }
    }

    fn search(
        &self,
        query: &str,
        domains: &[TextDomain],
        k: usize,
    ) -> Vec<(u64, u64, f32)> {
        let pool = k.max(50);
        let mut ranked_lists: Vec<Vec<(u64, u64)>> = Vec::new();
        let mut scores: HashMap<(u64, u64), f32> = HashMap::new();
        for domain in domains {
            let hits = self.index_for(*domain).search(query, pool);
            if hits.is_empty() {
                continue;
            }
            ranked_lists.push(hits.iter().map(|(t, s, _)| (*t, *s)).collect());
            for (trace_id, span_id, score) in hits {
                *scores.entry((trace_id, span_id)).or_insert(0.0) += score;
            }
        }
        if ranked_lists.is_empty() {
            return Vec::new();
        }
        let fused = rrf_fuse(&ranked_lists, 60.0);
        fused
            .into_iter()
            .take(k)
            .map(|((trace_id, span_id), rrf_score)| {
                let bm25_score = scores.get(&(trace_id, span_id)).copied().unwrap_or(0.0);
                (trace_id, span_id, rrf_score + bm25_score * 0.001)
            })
            .collect()
    }

    fn index_for(&self, domain: TextDomain) -> &Bm25TextIndex {
        match domain {
            TextDomain::Input => &self.input,
            TextDomain::Output => &self.output,
            TextDomain::Logs => &self.logs,
            TextDomain::Tool => &self.tool,
            TextDomain::Model => &self.model,
            TextDomain::Agent => &self.agent,
        }
    }
}

fn text_domain_names_json(domains: &[TextDomain]) -> String {
    let names: Vec<String> = domains
        .iter()
        .map(|domain| format!(r#""{}""#, domain.as_str()))
        .collect();
    format!("[{}]", names.join(","))
}
