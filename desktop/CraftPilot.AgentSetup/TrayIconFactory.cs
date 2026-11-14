using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Reflection;
using System.Runtime.InteropServices;

namespace CraftPilot.AgentSetup;

public static class TrayIconFactory
{
    private const string LogoResource = "Assets.craftpilot-icon.png";
    private static readonly Lazy<Bitmap> Logo = new(LoadLogo);

    public static Icon Create(Color statusColor)
    {
        using var bitmap = CreateLogoBitmap(32);
        using var graphics = Graphics.FromImage(bitmap);
        graphics.SmoothingMode = SmoothingMode.AntiAlias;

        using var ring = new SolidBrush(Color.FromArgb(220, 8, 13, 17));
        graphics.FillEllipse(ring, 19, 19, 13, 13);

        using var dot = new SolidBrush(statusColor);
        graphics.FillEllipse(dot, 21, 21, 9, 9);

        using var pen = new Pen(Color.FromArgb(235, 255, 255, 255), 1);
        graphics.DrawEllipse(pen, 21, 21, 9, 9);

        return ToIcon(bitmap);
    }

    public static Icon CreateBrandIcon(int size)
    {
        using var bitmap = CreateLogoBitmap(size);
        return ToIcon(bitmap);
    }

    private static Bitmap CreateLogoBitmap(int size)
    {
        if (size <= 0)
        {
            throw new ArgumentOutOfRangeException(nameof(size));
        }

        var logo = Logo.Value;
        var bitmap = new Bitmap(size, size, PixelFormat.Format32bppArgb);
        using var graphics = Graphics.FromImage(bitmap);
        graphics.SmoothingMode = SmoothingMode.AntiAlias;
        graphics.InterpolationMode = InterpolationMode.HighQualityBicubic;
        graphics.PixelOffsetMode = PixelOffsetMode.HighQuality;
        graphics.Clear(Color.Transparent);

        var scale = Math.Min((float)size / logo.Width, (float)size / logo.Height);
        var width = Math.Max(1, (int)Math.Round(logo.Width * scale));
        var height = Math.Max(1, (int)Math.Round(logo.Height * scale));
        graphics.DrawImage(
            logo,
            new Rectangle((size - width) / 2, (size - height) / 2, width, height),
            new Rectangle(0, 0, logo.Width, logo.Height),
            GraphicsUnit.Pixel);

        return bitmap;
    }

    private static Icon ToIcon(Bitmap bitmap)
    {
        var handle = bitmap.GetHicon();
        try
        {
            return (Icon)Icon.FromHandle(handle).Clone();
        }
        finally
        {
            DestroyIcon(handle);
        }
    }

    private static Bitmap LoadLogo()
    {
        using var stream = Assembly.GetExecutingAssembly().GetManifestResourceStream(LogoResource)
            ?? throw new InvalidOperationException("CraftPilot-loggan saknas i appen.");
        using var source = Image.FromStream(stream);
        return new Bitmap(source);
    }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool DestroyIcon(IntPtr handle);
}
