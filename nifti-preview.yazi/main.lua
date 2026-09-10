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
local helper_permissions_ready = package.config:sub(1, 1) == "\\"

local entry_context = ya.sync(function()
  local hovered = cx.active.current.hovered
  return hovered and tostring(hovered.url),
      hovered and tostring(hovered.path),
      hovered and hovered.name and tostring(hovered.name),
      hovered and hovered.url.spec.is_regular,
      hovered and hovered.cha.len,
      cx.active.preview.skip
end)

local function message(job, text)
  ya.preview_widget(job, ui.Text(text):area(job.area):align(ui.Align.CENTER):wrap(ui.Wrap.YES))
end

local function path_ready(path, is_regular, expected_len)
  local cha = fs.cha(Url(path))
  if not cha then
    return false
  end
  if is_regular then
    return true
  end
  return cha.len == expected_len
end

local function content_ready(file)
  return path_ready(file.path, file.url.spec.is_regular, file.cha.len)
end

local function ensure_helper_permissions()
  if helper_permissions_ready then
    return nil
  end
  local output, err = Command("chmod")
      :arg({ "u+x", HELPER })
      :stdout(Command.PIPED)
      :stderr(Command.PIPED)
      :output()
  if not output then
    return string.format("Failed to set execute permission on bundled helper: %s", tostring(err))
  end
  if not output.status.success then
    local detail = output.stderr:gsub("%s+$", "")
    return detail ~= "" and detail or "Failed to set execute permission on bundled helper"
  end
  helper_permissions_ready = true
  return nil
end

local function run(args)
  local permission_error = ensure_helper_permissions()
  if permission_error then
    return nil, permission_error
  end
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
  if not content_ready(job.file) then
    local text = job.file.url.spec.is_regular
        and "NIfTI file is not available"
        or "Remote NIfTI file, download to preview"
    message(job, text)
    return
  end
  local args = { "render", "--input", tostring(job.file.path) }
  if job.file.name then
    args[#args + 1] = "--name"
    args[#args + 1] = tostring(job.file.name)
  end
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
  if not content_ready(job.file) then
    return
  end
  self:seek_file(job.file.url, job.file.path, job.file.name, cx.active.preview.skip, job.units)
end

function M:seek_file(url, path, name, current, units)
  local args = { "probe", "--input", tostring(path) }
  if name then
    args[#args + 1] = "--name"
    args[#args + 1] = tostring(name)
  end
  local result = run(args)
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
  local url, path, name, is_regular, expected_len, current = entry_context()
  if not url or not path then
    return
  end
  local lower = url:lower()
  if lower:match("%.nii$") or lower:match("%.nii%.gz$") then
    if not path_ready(path, is_regular, expected_len) then
      return
    end
    self:seek_file(Url(url), path, name, current, units)
  else
    ya.emit("seek", { units * 5 })
  end
end

function M:preload(job)
  if not content_ready(job.file) then
    return false
  end
  local args = { "probe", "--input", tostring(job.file.path) }
  if job.file.name then
    args[#args + 1] = "--name"
    args[#args + 1] = tostring(job.file.name)
  end
  local result = run(args)
  return result ~= nil
end

return M
