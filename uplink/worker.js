// logOS uplink: the boss brain's front door, hosted on Cloudflare.
//
// logOS sends what the person said plus its current state; this Worker asks a strong free
// model (the same keyless engines RobinTHood uses) what to do, and returns plain text to show
// and a list of tool calls. logOS checks and clamps every tool call itself, so the model can
// only ever turn the knobs logOS offers.
//
// POST /v1/think  { "text": "...", "state": { ... } }  ->  { "say": "...", "do": [ ... ], "brain": "..." }

const OVH = 'https://oai.endpoints.kepler.ai.cloud.ovh.net/v1/chat/completions';
const KILO = 'https://ai-gateway.kilo.ai/api/v1/chat/completions';

// Tried in order. All keyless.
const ENGINES = [
  { label: 'GPT-OSS 120B', model: 'gpt-oss-120b', url: OVH },
  { label: 'Qwen 3.8 27B', model: 'Qwen3.8-27B', url: OVH },
  { label: 'Qwen 3.5 397B', model: 'Qwen3.5-397B-A17B', url: OVH },
  { label: 'Cohere North Mini', model: 'cohere/north-mini-code:free', url: KILO },
];

export const TOOLS = {
  set_detail: { args: 'level 1-4', about: 'render sharpness; 1 = blocky and fast, 4 = sharpest' },
  set_speed: { args: 'multiplier 0-3', about: 'animation speed; 0 freezes the dreamscape' },
  set_warmth: { args: 'value -1 to 1', about: 'colour mood; -1 cool blues, 1 warm pinks and golds' },
  set_swirl: { args: 'value 1-6', about: 'how tightly the spiral twists' },
  story: { args: 'a short opening line', about: 'ask the small on-board model to write a story from that opening' },
};

function systemPrompt() {
  const tools = Object.entries(TOOLS).map(([n, t]) => `- ${n} (${t.args}): ${t.about}`).join('\n');
  return `You are the mind in charge of logOS, an experimental operating system that runs directly on the hardware with no Linux or other OS underneath. A tiny on-board model handles booting; you are the stronger brain it reached over its uplink. Talk to the person in short, plain, friendly sentences (at most 3), using only plain ASCII characters.

You act on the machine only through these tools:
${tools}

Reply with ONLY a JSON object, no other text:
{"say": "what to show the person", "do": [{"tool": "set_detail", "value": 3}]}
Use "do": [] when no tool fits. For "story", "value" is the opening line as a string. Never claim you did something you did not do with a tool. If asked for something no tool can do yet (new drivers, files, network, sound), say so honestly and say it is on the roadmap.`;
}

const ascii = (s) =>
  String(s ?? '')
    .normalize('NFKD')
    .replace(/[‘’]/g, "'")
    .replace(/[“”]/g, '"')
    .replace(/[–—]/g, '-')
    .replace(/[^\x20-\x7e]/g, '')
    .slice(0, 400);

function parseReply(text) {
  const clean = String(text).replace(/<think>[\s\S]*?<\/think>/g, '');
  const m = clean.match(/\{[\s\S]*\}/);
  if (!m) return { say: ascii(clean), do: [] };
  try {
    const j = JSON.parse(m[0]);
    const calls = Array.isArray(j.do) ? j.do : [];
    return {
      say: ascii(j.say),
      do: calls
        .filter((c) => c && typeof c.tool === 'string' && TOOLS[c.tool])
        .slice(0, 4)
        .map((c) => ({ tool: c.tool, value: typeof c.value === 'number' ? c.value : ascii(c.value).slice(0, 80) })),
    };
  } catch {
    return { say: ascii(clean), do: [] };
  }
}

async function ask(text, state) {
  const messages = [
    { role: 'system', content: systemPrompt() },
    { role: 'user', content: `logOS state: ${JSON.stringify(state).slice(0, 400)}\nThe person said: ${text}` },
  ];
  const errors = [];
  for (const e of ENGINES) {
    try {
      const res = await fetch(e.url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'User-Agent': 'logOS-uplink/0.1' },
        body: JSON.stringify({ model: e.model, messages, stream: false, max_tokens: 600 }),
        signal: AbortSignal.timeout(25000),
      });
      if (!res.ok) {
        errors.push(`${e.label}: HTTP ${res.status}`);
        continue;
      }
      const j = await res.json();
      const content = j?.choices?.[0]?.message?.content;
      if (!content) {
        errors.push(`${e.label}: empty`);
        continue;
      }
      return { ...parseReply(content), brain: e.label };
    } catch (err) {
      errors.push(`${e.label}: ${String(err).slice(0, 60)}`);
    }
  }
  return { say: 'My uplink is up, but every boss brain is busy right now. Try again in a minute.', do: [], brain: 'none', errors };
}

const json = (status, body) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' } });

export default {
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === '/' || url.pathname === '/health') {
      return json(200, { ok: true, service: 'logOS uplink', tools: Object.keys(TOOLS), engines: ENGINES.map((e) => e.label) });
    }
    if (url.pathname === '/v1/think' && request.method === 'POST') {
      let body;
      try {
        body = await request.json();
      } catch {
        return json(400, { error: 'send JSON: {"text": "...", "state": {...}}' });
      }
      const text = ascii(body?.text).slice(0, 300);
      if (!text) return json(400, { error: 'text is required' });
      const reply = await ask(text, body?.state ?? {});
      if (url.searchParams.get('fmt') === 'lines') {
        // logOS's own wire format, so it needs no JSON parser.
        const lines = [`@@SAY ${reply.say.replace(/\s+/g, ' ')}`, ...reply.do.map((c) => `@@DO ${c.tool} ${c.value}`), '@@DONE'];
        return new Response(lines.join('\n') + '\n', { headers: { 'Content-Type': 'text/plain', 'X-Brain': reply.brain } });
      }
      return json(200, reply);
    }
    return json(404, { error: 'not found' });
  },
};
