using System.IO;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace VoicePrompt.IconGen;

/// <summary>
/// Renders <c>assets/voice-cloud.svg</c> (the microphone-cloud identity shared with the
/// Android app) into the Windows app icon set: a multi-resolution <c>app.ico</c> for the
/// executable and tray, plus a 256 px <c>app.png</c> for in-app display.
///
/// The geometry below is a 1:1 transcription of the SVG paths (WPF's path mini-language is
/// a superset of the SVG path grammar), so both platforms show the same mark.
/// Run with: <c>dotnet run --project windows/tools/IconGen -- &lt;outputDirectory&gt;</c>
/// </summary>
internal static class Program
{
    private const double DesignSize = 256.0;

    private static readonly int[] IcoSizes = { 16, 20, 24, 32, 40, 48, 64, 128, 256 };

    [STAThread]
    private static int Main(string[] args)
    {
        var outputDir = args.Length > 0
            ? args[0]
            : Path.Combine(AppContext.BaseDirectory, "out");
        Directory.CreateDirectory(outputDir);

        var icoPath = Path.Combine(outputDir, "app.ico");
        var pngPath = Path.Combine(outputDir, "app.png");

        File.WriteAllBytes(icoPath, BuildIco(IcoSizes));
        File.WriteAllBytes(pngPath, EncodePng(Render(256)));

        Console.WriteLine($"Wrote {icoPath} ({new FileInfo(icoPath).Length} bytes)");
        Console.WriteLine($"Wrote {pngPath} ({new FileInfo(pngPath).Length} bytes)");
        return 0;
    }

    private static RenderTargetBitmap Render(int size)
    {
        var scale = size / DesignSize;
        var visual = new DrawingVisual();

        using (var dc = visual.RenderOpen())
        {
            dc.PushTransform(new ScaleTransform(scale, scale));

            var black = Brush("#111111");
            var gray = Brush("#6B7280");
            var light = Brush("#E5E7EB");
            var orange = Brush("#F97316");

            // Cloud silhouette.
            dc.DrawGeometry(
                light,
                RoundPen(black, 6),
                Geometry.Parse("M74 168 a40 40 0 0 1 4 -79 a52 52 0 0 1 100 -8 a36 36 0 0 1 6 87 z"));

            // Microphone capsule.
            dc.DrawRoundedRectangle(black, null, new Rect(108, 70, 40, 78), 20, 20);

            // Capsule grille lines (accent).
            foreach (var y in new double[] { 90, 104, 118 })
            {
                dc.DrawLine(RoundPen(orange, 4), new Point(116, y), new Point(140, y));
            }

            // Microphone cradle.
            dc.DrawGeometry(null, RoundPen(gray, 7), Geometry.Parse("M92 132 a36 36 0 0 0 72 0"));

            // Stand.
            dc.DrawLine(RoundPen(gray, 7), new Point(128, 168), new Point(128, 196));
            dc.DrawLine(RoundPen(gray, 7), new Point(104, 196), new Point(152, 196));

            // Sound waves rising into the cloud.
            dc.DrawGeometry(null, RoundPen(orange, 5), Geometry.Parse("M168 96 q10 -14 0 -28"));
            dc.DrawGeometry(null, RoundPen(orange, 5), Geometry.Parse("M182 100 q18 -20 0 -40"));

            dc.Pop();
        }

        var bitmap = new RenderTargetBitmap(size, size, 96, 96, PixelFormats.Pbgra32);
        bitmap.Render(visual);
        bitmap.Freeze();
        return bitmap;
    }

    private static SolidColorBrush Brush(string hex)
    {
        var brush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(hex)!);
        brush.Freeze();
        return brush;
    }

    private static Pen RoundPen(Brush brush, double thickness)
    {
        var pen = new Pen(brush, thickness)
        {
            StartLineCap = PenLineCap.Round,
            EndLineCap = PenLineCap.Round,
            LineJoin = PenLineJoin.Round,
        };
        pen.Freeze();
        return pen;
    }

    private static byte[] EncodePng(BitmapSource source)
    {
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(source));
        using var ms = new MemoryStream();
        encoder.Save(ms);
        return ms.ToArray();
    }

    /// <summary>
    /// Build a multi-resolution .ico. Sizes up to 64 px are stored as 32-bpp DIBs (maximum
    /// compatibility with GDI+/<c>NotifyIcon</c>); 128 and 256 use PNG compression.
    /// </summary>
    private static byte[] BuildIco(IReadOnlyList<int> sizes)
    {
        var images = new List<(int Size, byte[] Data, bool Png)>(sizes.Count);
        foreach (var size in sizes)
        {
            var bitmap = Render(size);
            images.Add(size >= 128
                ? (size, EncodePng(bitmap), true)
                : (size, EncodeDib(bitmap), false));
        }

        using var ms = new MemoryStream();
        using var w = new BinaryWriter(ms);

        w.Write((ushort)0);                 // reserved
        w.Write((ushort)1);                 // type: icon
        w.Write((ushort)images.Count);

        var offset = 6 + (16 * images.Count);
        foreach (var (size, data, _) in images)
        {
            w.Write((byte)(size >= 256 ? 0 : size));
            w.Write((byte)(size >= 256 ? 0 : size));
            w.Write((byte)0);               // palette entries
            w.Write((byte)0);               // reserved
            w.Write((ushort)1);             // colour planes
            w.Write((ushort)32);            // bits per pixel
            w.Write(data.Length);
            w.Write(offset);
            offset += data.Length;
        }

        foreach (var (_, data, _) in images)
        {
            w.Write(data);
        }

        w.Flush();
        return ms.ToArray();
    }

    /// <summary>BITMAPINFOHEADER + bottom-up BGRA pixels + an all-opaque AND mask.</summary>
    private static byte[] EncodeDib(BitmapSource source)
    {
        var width = source.PixelWidth;
        var height = source.PixelHeight;

        var converted = new FormatConvertedBitmap(source, PixelFormats.Bgra32, null, 0);
        var stride = width * 4;
        var pixels = new byte[stride * height];
        converted.CopyPixels(pixels, stride, 0);

        var maskStride = ((width + 31) / 32) * 4;

        using var ms = new MemoryStream();
        using var w = new BinaryWriter(ms);

        w.Write(40);                        // biSize
        w.Write(width);                     // biWidth
        w.Write(height * 2);                // biHeight (XOR + AND)
        w.Write((ushort)1);                 // biPlanes
        w.Write((ushort)32);                // biBitCount
        w.Write(0);                         // biCompression = BI_RGB
        w.Write(stride * height);           // biSizeImage
        w.Write(0);                         // biXPelsPerMeter
        w.Write(0);                         // biYPelsPerMeter
        w.Write(0);                         // biClrUsed
        w.Write(0);                         // biClrImportant

        for (var y = height - 1; y >= 0; y--)
        {
            w.Write(pixels, y * stride, stride);
        }

        w.Write(new byte[maskStride * height]);

        w.Flush();
        return ms.ToArray();
    }
}
