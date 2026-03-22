-- ironmem.nvim — Neovim integration for IronMem session memory
-- Communicates with ironmem via MCP stdio transport

local M = {}

M.config = {
  binary = vim.fn.expand("~/.ironmem/bin/ironmem"),
  auto_start = true,   -- auto session_start on VimEnter
  auto_end = true,     -- auto session_end on VimLeavePre
  record_events = true, -- record buffer writes as events
}

local job_id = nil
local session_id = nil
local request_id = 0
local pending = {}
local initialized = false

-- ── JSON-RPC helpers ────────────────────────────────────────────────

local function next_id()
  request_id = request_id + 1
  return request_id
end

local function send_request(method, params, callback)
  if not job_id then return end
  local id = next_id()
  local msg = vim.json.encode({
    jsonrpc = "2.0",
    id = id,
    method = method,
    params = params or vim.empty_dict(),
  })
  if callback then
    pending[id] = callback
  end
  vim.fn.chansend(job_id, msg .. "\n")
end

local function send_notification(method, params)
  if not job_id then return end
  local msg = vim.json.encode({
    jsonrpc = "2.0",
    method = method,
    params = params or vim.empty_dict(),
  })
  vim.fn.chansend(job_id, msg .. "\n")
end

-- ── MCP protocol ────────────────────────────────────────────────────

local function call_tool(name, arguments, callback)
  send_request("tools/call", {
    name = name,
    arguments = arguments,
  }, callback)
end

local function get_project()
  -- Use git root or cwd
  local git_root = vim.fn.systemlist("git rev-parse --show-toplevel 2>/dev/null")[1]
  if git_root and git_root ~= "" and not git_root:match("^fatal") then
    return git_root
  end
  return vim.fn.getcwd()
end

local function parse_tool_result(result)
  if result and result.content then
    for _, block in ipairs(result.content) do
      if block.text then
        local ok, data = pcall(vim.json.decode, block.text)
        if ok then return data end
      end
    end
  end
  return nil
end

-- ── Lifecycle ───────────────────────────────────────────────────────

local function on_stdout(_, data, _)
  for _, line in ipairs(data) do
    if line ~= "" then
      local ok, msg = pcall(vim.json.decode, line)
      if ok and msg then
        if msg.id and pending[msg.id] then
          pending[msg.id](msg.result, msg.error)
          pending[msg.id] = nil
        end
      end
    end
  end
end

local function on_exit(_, code, _)
  job_id = nil
  session_id = nil
  initialized = false
  if code ~= 0 then
    vim.notify("[ironmem] process exited with code " .. code, vim.log.levels.WARN)
  end
end

function M.start()
  if job_id then return end

  local bin = M.config.binary
  if vim.fn.executable(bin) ~= 1 then
    vim.notify("[ironmem] binary not found: " .. bin, vim.log.levels.ERROR)
    return
  end

  job_id = vim.fn.jobstart({ bin, "mcp" }, {
    on_stdout = on_stdout,
    on_exit = on_exit,
    stdout_buffered = false,
  })

  if job_id <= 0 then
    vim.notify("[ironmem] failed to start", vim.log.levels.ERROR)
    job_id = nil
    return
  end

  -- Initialize MCP
  send_request("initialize", {
    protocolVersion = "2024-11-05",
    capabilities = {},
    clientInfo = { name = "ironmem.nvim", version = "0.1.0" },
  }, function(result, err)
    if err then
      vim.notify("[ironmem] init error: " .. vim.inspect(err), vim.log.levels.ERROR)
      return
    end
    initialized = true
    send_notification("notifications/initialized")

    -- Auto start session
    if M.config.auto_start then
      M.session_start()
    end
  end)
end

function M.stop()
  if session_id and M.config.auto_end then
    M.session_end()
  end
  if job_id then
    vim.fn.jobstop(job_id)
    job_id = nil
  end
end

-- ── Tool wrappers ───────────────────────────────────────────────────

function M.session_start()
  call_tool("session_start", { project = get_project() }, function(result, err)
    if err then
      vim.notify("[ironmem] session_start error: " .. vim.inspect(err), vim.log.levels.WARN)
      return
    end
    local data = parse_tool_result(result)
    if data and data.session_id then
      session_id = data.session_id
      vim.notify("[ironmem] session started: " .. session_id, vim.log.levels.INFO)
    end
  end)
end

function M.session_end()
  if not session_id then return end
  call_tool("session_end", { session_id = session_id }, function(result, err)
    if err then
      vim.notify("[ironmem] session_end error: " .. vim.inspect(err), vim.log.levels.WARN)
      return
    end
    local data = parse_tool_result(result)
    if data then
      if data.memory_id then
        vim.notify("[ironmem] session compressed → memory " .. data.memory_id, vim.log.levels.INFO)
      elseif data.skipped then
        vim.notify("[ironmem] session ended (no events)", vim.log.levels.INFO)
      end
    end
    session_id = nil
  end)
end

function M.record_event(tool_name, input, output)
  if not session_id then return end
  call_tool("record_event", {
    session_id = session_id,
    project = get_project(),
    tool = tool_name,
    input = input,
    output = output,
  })
end

function M.status()
  call_tool("get_status", vim.empty_dict(), function(result, err)
    if err then
      vim.notify("[ironmem] " .. vim.inspect(err), vim.log.levels.WARN)
      return
    end
    local data = parse_tool_result(result)
    if data then
      vim.notify(string.format(
        "[ironmem] sessions=%d memories=%d observations=%d",
        data.sessions or 0, data.memories or 0, data.observations or 0
      ), vim.log.levels.INFO)
    end
  end)
end

function M.search(query)
  call_tool("search_memories", {
    query = query,
    project = get_project(),
    limit = 10,
  }, function(result, err)
    if err then
      vim.notify("[ironmem] " .. vim.inspect(err), vim.log.levels.WARN)
      return
    end
    local data = parse_tool_result(result)
    if data and data.memories then
      local lines = {}
      for _, m in ipairs(data.memories) do
        table.insert(lines, "── Memory " .. m.id .. " ──")
        table.insert(lines, m.summary)
        if m.tags then table.insert(lines, "Tags: " .. m.tags) end
        table.insert(lines, "")
      end
      if #lines == 0 then
        vim.notify("[ironmem] no results for: " .. query, vim.log.levels.INFO)
      else
        -- Show in a scratch buffer
        local buf = vim.api.nvim_create_buf(false, true)
        vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
        vim.api.nvim_buf_set_option(buf, "filetype", "markdown")
        vim.api.nvim_buf_set_option(buf, "bufhidden", "wipe")
        vim.cmd("split")
        vim.api.nvim_win_set_buf(0, buf)
      end
    end
  end)
end

-- ── Setup ───────────────────────────────────────────────────────────

function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", M.config, opts or {})

  -- Commands
  vim.api.nvim_create_user_command("IronMemStart", function() M.session_start() end, {})
  vim.api.nvim_create_user_command("IronMemEnd", function() M.session_end() end, {})
  vim.api.nvim_create_user_command("IronMemStatus", function() M.status() end, {})
  vim.api.nvim_create_user_command("IronMemSearch", function(cmd)
    M.search(cmd.args)
  end, { nargs = 1 })

  -- Autocommands
  local group = vim.api.nvim_create_augroup("IronMem", { clear = true })

  vim.api.nvim_create_autocmd("VimEnter", {
    group = group,
    callback = function()
      M.start()
    end,
  })

  vim.api.nvim_create_autocmd("VimLeavePre", {
    group = group,
    callback = function()
      M.stop()
    end,
  })

  -- Record buffer writes as events
  if M.config.record_events then
    vim.api.nvim_create_autocmd("BufWritePost", {
      group = group,
      callback = function(ev)
        local file = ev.file or vim.fn.expand("%:p")
        M.record_event("write", file)
      end,
    })
  end
end

return M
