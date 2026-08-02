// One-click connection snippets (MISSION.md §6F) — generated with the user's
// real base URL and, when revealed, a real key.

const PLACEHOLDER = "sk-starfish-YOUR-KEY";

export interface SnippetInput {
  baseUrl: string; // e.g. http://127.0.0.1:8787
  key?: string; // real secret when revealed
  model?: string; // agent id / alias
}

export function claudeCodeSnippet({ baseUrl, key }: SnippetInput): string {
  return `export ANTHROPIC_BASE_URL=${baseUrl}
export ANTHROPIC_AUTH_TOKEN=${key ?? PLACEHOLDER}
# Optional: route Claude Code's small/fast model to a different agent
# export ANTHROPIC_SMALL_FAST_MODEL=claude-3-5-haiku-latest
claude`;
}

export function codexSnippet({ baseUrl, key, model }: SnippetInput): string {
  return `# ~/.codex/config.toml
[model_providers.starfish]
name = "Starfish (Hyperagent)"
base_url = "${baseUrl}/v1"
env_key = "STARFISH_API_KEY"
wire_api = "responses"

[profiles.starfish]
model_provider = "starfish"
model = "${model ?? "hyperagent-default"}"

# then:
#   export STARFISH_API_KEY=${key ?? PLACEHOLDER}
#   codex --profile starfish`;
}

export function cursorSnippet({ baseUrl, key }: SnippetInput): string {
  return `Cursor → Settings → Models → OpenAI API Key
  API key:  ${key ?? PLACEHOLDER}
  Override OpenAI Base URL:  ${baseUrl}/v1

Pick your model by name from GET /v1/models (or use "hyperagent-default").`;
}

export function pythonSnippet({ baseUrl, key, model }: SnippetInput): string {
  return `from openai import OpenAI

client = OpenAI(base_url="${baseUrl}/v1", api_key="${key ?? PLACEHOLDER}")
r = client.chat.completions.create(
    model="${model ?? "hyperagent-default"}",
    messages=[{"role": "user", "content": "Research X and summarize."}],
)
print(r.choices[0].message.content)`;
}

export function anthropicSdkSnippet({ baseUrl, key, model }: SnippetInput): string {
  return `import anthropic

client = anthropic.Anthropic(base_url="${baseUrl}", api_key="${key ?? PLACEHOLDER}")
message = client.messages.create(
    model="${model ?? "hyperagent-default"}",
    max_tokens=1024,
    messages=[{"role": "user", "content": "Hello from the Anthropic SDK"}],
)
print(message.content[0].text)`;
}

export function curlSnippet({ baseUrl, key, model }: SnippetInput): string {
  const k = key ?? PLACEHOLDER;
  return `# OpenAI surface
curl ${baseUrl}/v1/chat/completions \\
  -H "Authorization: Bearer ${k}" -H "Content-Type: application/json" \\
  -d '{"model": "${model ?? "hyperagent-default"}", "messages": [{"role": "user", "content": "hi"}]}'

# Anthropic surface
curl ${baseUrl}/v1/messages \\
  -H "x-api-key: ${k}" -H "anthropic-version: 2023-06-01" -H "Content-Type: application/json" \\
  -d '{"model": "${model ?? "hyperagent-default"}", "max_tokens": 1024, "messages": [{"role": "user", "content": "hi"}]}'`;
}
