# Serves the files of C:\sandbox\pkg at http://localhost:8765/ so `winget install --manifest`
# can download the release ZIP inside the sandbox before there is a published release (F13.9).
# Leave it running in its own terminal; Ctrl+C stops it.

param([string]$Folder = 'C:\sandbox\pkg', [int]$Port = 8765)

$listener = [System.Net.HttpListener]::new()
$listener.Prefixes.Add("http://localhost:$Port/")
$listener.Start()
Write-Host "Serving $Folder at http://localhost:$Port/ (Ctrl+C stops)"
try {
    while ($listener.IsListening) {
        $context = $listener.GetContext()
        $name = [Uri]::UnescapeDataString($context.Request.Url.AbsolutePath.TrimStart('/'))
        $file = Join-Path $Folder $name
        if ($name -and -not $name.Contains('..') -and (Test-Path $file -PathType Leaf)) {
            $bytes = [System.IO.File]::ReadAllBytes($file)
            $context.Response.ContentType = 'application/octet-stream'
            $context.Response.ContentLength64 = $bytes.Length
            $context.Response.OutputStream.Write($bytes, 0, $bytes.Length)
            Write-Host "200 $name ($($bytes.Length) bytes)"
        } else {
            $context.Response.StatusCode = 404
            Write-Host "404 $name"
        }
        $context.Response.Close()
    }
} finally {
    $listener.Stop()
}
