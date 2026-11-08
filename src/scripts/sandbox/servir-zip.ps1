# Serves the files of C:\sandbox\pkg at http://localhost:8765/ so `winget install --manifest`
# can download the release ZIP inside the sandbox before there is a published release (F13.9).
# Leave it running in its own terminal; Ctrl+C stops it.
#
# winget may download through Delivery Optimization, which asks for parts of the file (`Range`),
# asks with HEAD first, and drops connections it no longer needs: every request is answered on
# its own, and one that the client drops is logged without stopping the server.

param([string]$Folder = 'C:\sandbox\pkg', [int]$Port = 8765)

$listener = [System.Net.HttpListener]::new()
$listener.Prefixes.Add("http://localhost:$Port/")
$listener.IgnoreWriteExceptions = $true
$listener.Start()
Write-Host "Serving $Folder at http://localhost:$Port/ (Ctrl+C stops)"
try {
    while ($listener.IsListening) {
        $context = $listener.GetContext()
        $request = $context.Request
        $response = $context.Response
        $name = [Uri]::UnescapeDataString($request.Url.AbsolutePath.TrimStart('/'))
        $range = $request.Headers['Range']
        $line = "$($request.HttpMethod) $name" + $(if ($range) { " ($range)" } else { '' })
        try {
            $file = Join-Path $Folder $name
            if (-not $name -or $name.Contains('..') -or -not (Test-Path $file -PathType Leaf)) {
                $response.StatusCode = 404
                Write-Host "404 $line"
                continue
            }
            $bytes = [System.IO.File]::ReadAllBytes($file)
            $start = 0L
            $end = $bytes.LongLength - 1
            $response.ContentType = 'application/octet-stream'
            $response.AddHeader('Accept-Ranges', 'bytes')
            if ($range -match '^bytes=(\d*)-(\d*)$') {
                if ($Matches[1]) {
                    $start = [long]$Matches[1]
                    if ($Matches[2]) { $end = [Math]::Min([long]$Matches[2], $end) }
                } elseif ($Matches[2]) {
                    $start = [Math]::Max(0L, $bytes.LongLength - [long]$Matches[2])
                }
                if ($start -gt $end) {
                    $response.StatusCode = 416
                    $response.AddHeader('Content-Range', "bytes */$($bytes.LongLength)")
                    Write-Host "416 $line"
                    continue
                }
                $response.StatusCode = 206
                $response.AddHeader('Content-Range', "bytes $start-$end/$($bytes.LongLength)")
            }
            $length = $end - $start + 1
            $response.ContentLength64 = $length
            if ($request.HttpMethod -ne 'HEAD') {
                $response.OutputStream.Write($bytes, [int]$start, [int]$length)
            }
            Write-Host "$($response.StatusCode) $line, $length bytes"
        } catch {
            Write-Host "dropped $line : $($_.Exception.InnerException.Message)"
        } finally {
            try { $response.Close() } catch { }
        }
    }
} finally {
    $listener.Stop()
}
