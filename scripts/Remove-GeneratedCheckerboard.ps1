[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$InputPath,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'

$sourcePath = (Resolve-Path -LiteralPath $InputPath).Path
$destinationDirectory = Split-Path -Parent $OutputPath
if (-not (Test-Path -LiteralPath $destinationDirectory)) {
    New-Item -ItemType Directory -Path $destinationDirectory | Out-Null
}

Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies 'System.Drawing' -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;

public static class GeneratedCheckerboardCleaner
{
    private static bool IsGeneratedBackground(Color c, bool darkBackground)
    {
        int max = Math.Max(c.R, Math.Max(c.G, c.B));
        int min = Math.Min(c.R, Math.Min(c.G, c.B));
        if (darkBackground) return max <= 24 && max - min <= 10;
        return min >= 232 && max - min <= 7;
    }

    public static void Clean(string inputPath, string outputPath)
    {
        using (var source = new Bitmap(inputPath))
        using (var output = new Bitmap(source.Width, source.Height, PixelFormat.Format32bppArgb))
        {
            int width = source.Width;
            int height = source.Height;
            int count = checked(width * height);
            Color corner = source.GetPixel(0, 0);
            bool darkBackground = corner.R + corner.G + corner.B < 192;
            var outside = new bool[count];
            var queue = new Queue<int>(Math.Min(count, 65536));

            Action<int, int> enqueue = (x, y) =>
            {
                int index = y * width + x;
                if (outside[index] || !IsGeneratedBackground(source.GetPixel(x, y), darkBackground)) return;
                outside[index] = true;
                queue.Enqueue(index);
            };

            for (int x = 0; x < width; x++)
            {
                enqueue(x, 0);
                enqueue(x, height - 1);
            }
            for (int y = 1; y < height - 1; y++)
            {
                enqueue(0, y);
                enqueue(width - 1, y);
            }

            while (queue.Count > 0)
            {
                int index = queue.Dequeue();
                int x = index % width;
                int y = index / width;
                if (x > 0) enqueue(x - 1, y);
                if (x + 1 < width) enqueue(x + 1, y);
                if (y > 0) enqueue(x, y - 1);
                if (y + 1 < height) enqueue(x, y + 1);
            }

            for (int y = 0; y < height; y++)
            {
                for (int x = 0; x < width; x++)
                {
                    int index = y * width + x;
                    Color color = source.GetPixel(x, y);
                    output.SetPixel(x, y, outside[index]
                        ? Color.FromArgb(0, color.R, color.G, color.B)
                        : Color.FromArgb(255, color.R, color.G, color.B));
                }
            }

            output.Save(outputPath, ImageFormat.Png);
        }
    }
}
'@

[GeneratedCheckerboardCleaner]::Clean($sourcePath, $OutputPath)

$result = [System.Drawing.Bitmap]::FromFile($OutputPath)
try {
    if ($result.PixelFormat -notmatch 'Argb' -or $result.GetPixel(0, 0).A -ne 0) {
        throw "Generated output does not contain the expected transparent alpha channel: $OutputPath"
    }
    [pscustomobject]@{
        path = (Resolve-Path -LiteralPath $OutputPath).Path
        width = $result.Width
        height = $result.Height
        pixelFormat = $result.PixelFormat.ToString()
        cornerAlpha = $result.GetPixel(0, 0).A
    }
}
finally {
    $result.Dispose()
}
