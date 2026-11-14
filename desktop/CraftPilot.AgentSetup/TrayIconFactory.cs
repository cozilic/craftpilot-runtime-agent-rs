using System.Drawing;
using System.Drawing.Drawing2D;
using System.Runtime.InteropServices;

namespace CraftPilot.AgentSetup;

public static class TrayIconFactory
{
    public static Icon Create(Color color)
    {
        using var bitmap = new Bitmap(16, 16);
        using var graphics = Graphics.FromImage(bitmap);
        graphics.SmoothingMode = SmoothingMode.AntiAlias;
        graphics.Clear(Color.Transparent);

        using var brush = new SolidBrush(color);
        graphics.FillEllipse(brush, 2, 2, 12, 12);

        using var pen = new Pen(Color.FromArgb(210, 255, 255, 255), 1);
        graphics.DrawEllipse(pen, 2, 2, 12, 12);

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

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool DestroyIcon(IntPtr handle);
}
