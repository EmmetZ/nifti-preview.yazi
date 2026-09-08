local M = {}

local function helper_path()
  local separator = package.config:sub(1, 1)
  local config_home = os.getenv("YAZI_CONFIG_HOME")
  if not config_home or config_home == "" then
    if separator == "\\" then
      local app_data = os.getenv("APPDATA")
      config_home = app_data and (app_data .. "\\yazi\\config") or "."
    else
      local xdg_config = os.getenv("XDG_CONFIG_HOME")
      local home = os.getenv("HOME") or "."
      config_home = (xdg_config and xdg_config ~= "" and xdg_config or (home .. "/.config")) .. "/yazi"
    end
  end
  local executable = separator == "\\" and "yazi-nifti-preview.exe" or "yazi-nifti-preview"
  return table.concat({ config_home, "plugins", "nifti-preview.yazi", "assets", executable }, separator)
end

local HELPER = helper_path()

local entry_context = ya.sync(function()
  local hovered = cx.active.current.hovered
  return hovered and tostring(hovered.url), cx.active.preview.skip
end)

local function message(job, text)
  ya.preview_widgets(job, {
    ui.Text(text):align(ui.Text.CENTER):area(job.area),
  })
end

local function run(args)
  local output, err = Command(HELPER)
      :arg(args)
      :stdout(Command.PIPED)
      :stderr(Command.PIPED)
      :output()
  if not output then
    return nil, string.format("Failed to start bundled helper %s: %s", HELPER, tostring(err))
  end
  if not output.status.success then
    local detail = output.stderr:gsub("%s+$", "")
    return nil, detail ~= "" and detail or "NIfTI preview failed"
  end
  local decoded = ya.json_decode(output.stdout)
  if not decoded then
    return nil, "Invalid response from yazi-nifti-preview"
  end
  return decoded, nil
end

function M:peek(job)
  local args = { "render", "--input", tostring(job.file.url) }
  if job.skip > 0 then
    args[#args + 1] = "--slice"
    args[#args + 1] = tostring(job.skip - 1)
  end
  local result, err = run(args)
  if not result then
    message(job, err)
    return
  end
  ya.image_show(Url(result.image), job.area)
end

function M:seek(job)
  self:seek_url(job.file.url, cx.active.preview.skip, job.units)
end

function M:seek_url(url, current, units)
  local result = run({ "probe", "--input", tostring(url) })
  if not result then
    return
  end
  if current == 0 then
    current = result.default_slice + 1
  end
  local step = ya.clamp(-1, units, 1)
  local target = ya.clamp(1, current + step, result.slice_count)
  ya.emit("peek", { target, only_if = url })
end

function M:entry(job)
  local units = tonumber(job.args[1]) or 0
  local url, current = entry_context()
  if not url then
    return
  end
  local lower = url:lower()
  if lower:match("%.nii$") or lower:match("%.nii%.gz$") then
    self:seek_url(Url(url), current, units)
  else
    ya.emit("seek", { units * 5 })
  end
end

function M:preload(job)
  run({ "probe", "--input", tostring(job.file.url) })
end

return M
