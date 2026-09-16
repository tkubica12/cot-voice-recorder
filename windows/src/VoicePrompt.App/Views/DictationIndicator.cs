using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Threading;
using Brushes = System.Windows.Media.Brushes;
using Color = System.Windows.Media.Color;
using Orientation = System.Windows.Controls.Orientation;

namespace VoicePrompt.App.Views;

public sealed class DictationIndicator : Window
{
    private readonly TextBlock _text = new()
    {
        Foreground = Brushes.White,
        FontSize = 12,
        VerticalAlignment = VerticalAlignment.Center,
        Margin = new Thickness(10, 0, 10, 0),
    };
    private readonly System.Windows.Shapes.Ellipse _dot = new()
    {
        Width = 8,
        Height = 8,
        Fill = Brushes.OrangeRed,
        VerticalAlignment = VerticalAlignment.Center,
        Margin = new Thickness(12, 0, 0, 0),
    };
    private readonly TextBlock _preview = new()
    {
        Foreground = Brushes.White, FontSize = 14, TextWrapping = TextWrapping.Wrap,
        TextTrimming = TextTrimming.CharacterEllipsis, Height = 60, LineHeight = 20,
        LineStackingStrategy = LineStackingStrategy.BlockLineHeight, Margin = new Thickness(12, 8, 12, 12),
    };
    private readonly DispatcherTimer _dismiss = new() { Interval = TimeSpan.FromMilliseconds(900) };

    public DictationIndicator()
    {
        Title = "VoicePrompt dictation";
        Width = 440;
        Height = 112;
        WindowStyle = WindowStyle.None;
        ResizeMode = ResizeMode.NoResize;
        ShowInTaskbar = false;
        ShowActivated = false;
        Focusable = false;
        Topmost = true;
        AllowsTransparency = true;
        Background = Brushes.Transparent;
        var header = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 10, 0, 0) };
        header.Children.Add(_dot);
        header.Children.Add(_text);
        var panel = new StackPanel();
        panel.Children.Add(header);
        panel.Children.Add(_preview);
        _dismiss.Tick += (_, _) => { _dismiss.Stop(); Hide(); };
        Content = new Border
        {
            CornerRadius = new CornerRadius(17),
            Background = new SolidColorBrush(Color.FromRgb(30, 30, 34)),
            Child = panel,
        };
        SourceInitialized += (_, _) =>
        {
            var hwnd = new WindowInteropHelper(this).Handle;
            SetWindowLongPtr(hwnd, -20, new IntPtr(GetWindowLongPtr(hwnd, -20).ToInt64()
                | 0x08000000 | 0x80 | 0x20)); // NOACTIVATE, TOOLWINDOW, TRANSPARENT
        };
    }

    public void Present(string text, double level = 0, bool recording = true, string preview = "",
        bool dismiss = false)
    {
        _dismiss.Stop();
        _text.Text = text;
        _preview.Text = string.IsNullOrWhiteSpace(preview) ? "Recognized words will appear here as you speak." : preview;
        _dot.Fill = recording ? Brushes.OrangeRed : Brushes.DodgerBlue;
        _dot.Opacity = recording ? 0.5 + level * 0.5 : 1;
        var area = SystemParameters.WorkArea;
        Left = area.Left + (area.Width - Width) / 2;
        Top = area.Bottom - Height - 18;
        if (!IsVisible) Show();
        if (dismiss) _dismiss.Start();
    }

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);
    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern IntPtr SetWindowLongPtr(IntPtr hwnd, int index, IntPtr value);
}
